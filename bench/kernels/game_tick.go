// A game server's tick, the Go twin of game_tick.fen: entities in a slice, each with an
// inventory in a map, the players' commands through a buffered channel from their own
// goroutine, the grid's cells split across one goroutine for each of GOMAXPROCS and put back
// together in order, and a snapshot of what changed written in std.pack's compact form:
// zigzag varints after a count, which encoding/binary writes.
package main

import (
	"encoding/binary"
	"fmt"
	"os"
	"runtime"
	"sync"
	"time"
)

const (
	ticks    = 40
	commands = 64
	size     = 2048
	cell     = 32
	side     = 64
	reach    = 1024
)

var items = [...]string{"gold", "arrow", "potion", "bone", "herb", "gem", "scroll", "key"}

type Entity struct {
	number    int64
	x, y      int64
	vx, vy    int64
	health    int64
	inventory map[string]int64
	changed   bool
}

// What a client is sent about an entity that changed.
type Snap struct {
	number, x, y, health, carried int64
}

type Kind int

const (
	Move Kind = iota
	Give
	Heal
)

// A player's command: a move sets the entity's speed to (a, b), a gift gives b of item a, and a
// heal adds a to its health.
type Command struct {
	kind Kind
	who  int
	a, b int64
}

// The next state of a small linear congruential generator, the same on every machine.
func next(seed int64) int64 {
	return (seed*1_103_515_245 + 12_345) % 2_147_483_648
}

// A number from 0 up to top taken from a generator's state.
func below(seed, top int64) int64 {
	return seed / 65_536 % top
}

// The players: every tick's commands, sent in order from their own goroutine.
func players(out chan<- Command, seed, entities int64) {
	state := seed
	for range ticks * commands {
		state = next(state)
		who := int(below(state, entities))
		state = next(state)
		kind := below(state, 3)
		state = next(state)
		a := below(state, 17)
		state = next(state)
		b := below(state, 17)
		switch kind {
		case 0:
			out <- Command{Move, who, a - 8, b - 8}
		case 1:
			out <- Command{Give, who, a % int64(len(items)), b + 1}
		default:
			out <- Command{Heal, who, a, 0}
		}
	}
}

func apply(entities []Entity, command Command) {
	entity := &entities[command.who]
	switch command.kind {
	case Move:
		entity.vx, entity.vy = command.a, command.b
	case Give:
		entity.inventory[items[command.a]] += command.b
		entity.changed = true
	case Heal:
		entity.health = min(entity.health+command.a, 100)
		entity.changed = true
	}
}

// Moves one axis, bouncing off the world's edges.
func moved(at, speed int64) (int64, int64) {
	there := at + speed
	if there < 0 {
		return -there, -speed
	}
	if there >= size {
		return 2*(size-1) - there, -speed
	}
	return there, speed
}

func integrate(entities []Entity) {
	for i := range entities {
		entity := &entities[i]
		entity.changed = false
		if entity.vx != 0 || entity.vy != 0 {
			entity.x, entity.vx = moved(entity.x, entity.vx)
			entity.y, entity.vy = moved(entity.y, entity.vy)
			entity.changed = true
		}
	}
}

// What the entities in one cell take from their neighbours within reach: damage from other
// teams, closer hurting more, and a little healing from their own.
func cellHealth(at int64, cells [][]int, xs, ys []int64) []int64 {
	gx, gy := at%side, at/side
	changes := make([]int64, 0, len(cells[at]))
	for _, me := range cells[at] {
		var change int64
		for ny := max(gy-1, 0); ny < min(gy+2, side); ny++ {
			for nx := max(gx-1, 0); nx < min(gx+2, side); nx++ {
				for _, other := range cells[ny*side+nx] {
					dx := xs[other] - xs[me]
					dy := ys[other] - ys[me]
					near := dx*dx + dy*dy
					if other != me && near < reach {
						if other%4 == me%4 {
							change++
						} else {
							change -= 1 + (reach-near)/256
						}
					}
				}
			}
		}
		changes = append(changes, change)
	}
	return changes
}

func packed(snaps []Snap) []byte {
	out := binary.AppendUvarint(nil, uint64(len(snaps)))
	for _, snap := range snaps {
		out = binary.AppendVarint(out, snap.number)
		out = binary.AppendVarint(out, snap.x)
		out = binary.AppendVarint(out, snap.y)
		out = binary.AppendVarint(out, snap.health)
		out = binary.AppendVarint(out, snap.carried)
	}
	return out
}

func carried(entity *Entity) int64 {
	var total int64
	for _, count := range entity.inventory {
		total += count
	}
	return total
}

// One tick after the commands: move, file everyone in the grid, take health on goroutines,
// and pack what changed. Gives the snapshot's checksum.
func tick(entities []Entity, cells [][]int, parts int, turn int64) int64 {
	integrate(entities)
	for at := range cells {
		cells[at] = cells[at][:0]
	}
	xs := make([]int64, 0, len(entities))
	ys := make([]int64, 0, len(entities))
	for _, entity := range entities {
		xs = append(xs, entity.x)
		ys = append(ys, entity.y)
		at := entity.y/cell*side + entity.x/cell
		cells[at] = append(cells[at], int(entity.number))
	}
	const total = side * side
	share := (total + int64(parts) - 1) / int64(parts)
	found := make([][][]int64, parts)
	var running sync.WaitGroup
	for part := range parts {
		running.Go(func() {
			first, last := int64(part)*share, min(int64(part+1)*share, total)
			for at := first; at < last; at++ {
				found[part] = append(found[part], cellHealth(at, cells, xs, ys))
			}
		})
	}
	running.Wait()
	at := 0
	for _, part := range found {
		for _, changes := range part {
			for place, me := range cells[at] {
				change := changes[place]
				if change == 0 {
					continue
				}
				entity := &entities[me]
				entity.health = min(entity.health+change, 100)
				if entity.health <= 0 {
					// Dies, drops half its gold, and comes back somewhere else.
					entity.inventory["gold"] /= 2
					entity.inventory["bone"]++
					entity.health = 100
					entity.x = (int64(me)*7_919 + turn*104_729) % size
					entity.y = (int64(me)*104_729 + turn*7_919) % size
				}
				entity.changed = true
			}
			at++
		}
	}
	var snaps []Snap
	for i := range entities {
		entity := &entities[i]
		if entity.changed {
			snaps = append(snaps, Snap{entity.number, entity.x, entity.y, entity.health, carried(entity)})
		}
	}
	data := packed(snaps)
	hash := int64(len(data))
	for _, b := range data {
		hash = (hash*31 + int64(b)) % 1_000_000_007
	}
	return hash
}

func main() {
	started := time.Now()
	const count = 10_000
	// One part of the grid for each thread Go runs goroutines on: GOMAXPROCS, which is one for
	// each core unless it's set.
	parts := runtime.GOMAXPROCS(0)
	entities := make([]Entity, 0, count)
	state := int64(42)
	for number := range int64(count) {
		state = next(state)
		x := below(state, size)
		state = next(state)
		y := below(state, size)
		state = next(state)
		var speed int64
		if number%4 == 0 {
			speed = below(state, 9) - 4
		}
		entities = append(entities, Entity{
			number:    number,
			x:         x,
			y:         y,
			vx:        speed,
			vy:        -speed,
			health:    100,
			inventory: map[string]int64{"gold": number % 50},
		})
	}
	cells := make([][]int, side*side)
	var checksum int64
	sent := make(chan Command, 64)
	go players(sent, 7, count)
	for turn := range int64(ticks) {
		for range commands {
			apply(entities, <-sent)
		}
		checksum = (checksum*7 + tick(entities, cells, parts, turn)) % 1_000_000_007
	}
	var health, held int64
	for i := range entities {
		entity := &entities[i]
		health += entity.health
		held += carried(entity) * (entity.number%7 + 1)
	}
	fmt.Println(checksum, health, held)
	if len(os.Args) > 1 {
		fmt.Printf("kernel-ns %d\n", time.Since(started).Nanoseconds())
	}
}
