// A chat server's core, the Go twin of chat_server.fen: a hub goroutine owning the rooms in a
// map, fanning each message out over buffered channels to four shard goroutines, each owning
// its users' mailboxes in a map. Each gives back what it found once its channel is closed.
package main

import (
	"cmp"
	"fmt"
	"os"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"
)

const (
	users   = 400
	rooms   = 40
	history = 50
	kept    = 16
	shards  = 4
)

var words = [...]string{"hey", "anyone", "seen", "the", "new", "map", "lag", "again", "gg", "raid", "at", "nine"}

type Kind int

const (
	Join Kind = iota
	Leave
	Say
	Whisper
)

// What a client sends, with the room by number. A whisper goes to user to, not to a room.
type Event struct {
	kind Kind
	user int64
	room int64
	to   int64
	text string
}

// A message on its way to one user's mailbox.
type Message struct {
	user   int64
	room   string
	author int64
	text   string
}

// One room, as the hub keeps it.
type Room struct {
	members []int64
	history []string
	said    int64
}

// One user's mailbox, as a shard keeps it.
type Mailbox struct {
	received int64
	bytes    int64
	mentions int64
	recent   []string
}

// What a shard gives back about one of its users.
type Summary struct {
	user     int64
	received int64
	bytes    int64
	mentions int64
	last     string
}

// How busy a room was.
type Busy struct {
	said int64
	name string
}

// What the hub gives back when every event has been handled.
type HubReport struct {
	refused int64
	rooms   []Busy
	history int
}

// The next state of a small linear congruential generator, the same on every machine.
func next(seed int64) int64 {
	return (seed*1_103_515_245 + 12_345) % 2_147_483_648
}

// A number from 0 up to top taken from a generator's state.
func below(seed, top int64) int64 {
	return seed / 65_536 % top
}

// Every client's event, in the order the server takes them.
func events(seed, count int64) []Event {
	state := seed
	made := make([]Event, 0, count)
	for number := range count {
		state = next(state)
		roll := below(state, 100)
		state = next(state)
		user := below(state, users)
		state = next(state)
		room := below(state, rooms)
		switch {
		case roll < 14:
			made = append(made, Event{kind: Join, user: user, room: room})
		case roll < 18:
			made = append(made, Event{kind: Leave, user: user, room: room})
		default:
			state = next(state)
			first := below(state, 1_000)
			length := 1 + below(state, 6)
			mention := int64(-1)
			if roll%10 == 0 {
				mention = below(next(state), users)
			}
			text := rendered(number, first, length, mention)
			if roll < 22 {
				made = append(made, Event{kind: Whisper, user: user, to: below(next(state), users), text: text})
			} else {
				made = append(made, Event{kind: Say, user: user, room: room, text: text})
			}
		}
	}
	return made
}

// What a client said, as text.
func rendered(number, first, count, mention int64) string {
	said := make([]string, 0, count+1)
	for at := range count {
		said = append(said, words[(first+at*7)%int64(len(words))])
	}
	if mention >= 0 {
		said = append(said, "@u"+strconv.FormatInt(mention, 10))
	}
	return strconv.FormatInt(number, 10) + ": " + strings.Join(said, " ")
}

// A hash of a line's bytes.
func hashed(line string) int64 {
	hash := int64(len(line))
	for i := 0; i < len(line); i++ {
		hash = (hash*31 + int64(line[i])) % 1_000_000_007
	}
	return hash
}

// The hub: owns the rooms, checks each event and fans each message out to the shards, whose
// channels it closes once its own is.
func hub(inbox <-chan Event, outs []chan Message) HubReport {
	defer func() {
		for _, out := range outs {
			close(out)
		}
	}()
	owned := map[string]*Room{}
	var refused int64
	for event := range inbox {
		switch event.kind {
		case Join:
			name := "room-" + strconv.FormatInt(event.room, 10)
			room, found := owned[name]
			if !found {
				room = &Room{}
				owned[name] = room
			}
			if slices.Contains(room.members, event.user) {
				refused++
			} else {
				room.members = append(room.members, event.user)
			}
		case Leave:
			room, found := owned["room-"+strconv.FormatInt(event.room, 10)]
			if !found || !slices.Contains(room.members, event.user) {
				refused++
				continue
			}
			room.members = slices.DeleteFunc(room.members, func(member int64) bool { return member == event.user })
		case Say:
			name := "room-" + strconv.FormatInt(event.room, 10)
			room, found := owned[name]
			if !found || !slices.Contains(room.members, event.user) {
				refused++
				continue
			}
			room.said++
			room.history = append(room.history, "u"+strconv.FormatInt(event.user, 10)+": "+event.text)
			if len(room.history) > 2*history {
				room.history = append(room.history[:0], room.history[history:]...)
			}
			for _, member := range room.members {
				if member != event.user {
					outs[member%shards] <- Message{user: member, room: name, author: event.user, text: event.text}
				}
			}
		case Whisper:
			outs[event.to%shards] <- Message{user: event.to, room: "dm", author: event.user, text: event.text}
		}
	}
	busiest := make([]Busy, 0, len(owned))
	lines := 0
	for name, room := range owned {
		busiest = append(busiest, Busy{room.said, name})
		lines += len(room.history)
	}
	slices.SortFunc(busiest, func(a, b Busy) int {
		return cmp.Or(cmp.Compare(b.said, a.said), strings.Compare(a.name, b.name))
	})
	return HubReport{refused: refused, rooms: busiest[:min(5, len(busiest))], history: lines}
}

// A shard: the mailboxes of the users it owns. Gives back what each got once its inbox closes.
func shard(inbox <-chan Message) []Summary {
	boxes := map[int64]*Mailbox{}
	for message := range inbox {
		line := fmt.Sprintf("[%s] u%d: %s", message.room, message.author, message.text)
		mailbox, found := boxes[message.user]
		if !found {
			mailbox = &Mailbox{}
			boxes[message.user] = mailbox
		}
		mailbox.received++
		mailbox.bytes += int64(len(line))
		if strings.Contains(message.text, "@u"+strconv.FormatInt(message.user, 10)) {
			mailbox.mentions++
		}
		mailbox.recent = append(mailbox.recent, line)
		if len(mailbox.recent) > 2*kept {
			mailbox.recent = append(mailbox.recent[:0], mailbox.recent[kept:]...)
		}
	}
	summaries := make([]Summary, 0, len(boxes))
	for user, mailbox := range boxes {
		last := ""
		if len(mailbox.recent) > 0 {
			last = mailbox.recent[len(mailbox.recent)-1]
		}
		summaries = append(summaries, Summary{user, mailbox.received, mailbox.bytes, mailbox.mentions, last})
	}
	return summaries
}

func main() {
	started := time.Now()
	sent := events(2026, 20_000)
	outs := make([]chan Message, shards)
	found := make([][]Summary, shards)
	var running sync.WaitGroup
	for at := range outs {
		outs[at] = make(chan Message, 256)
		running.Go(func() { found[at] = shard(outs[at]) })
	}
	toHub := make(chan Event, 64)
	reported := make(chan HubReport, 1)
	go func() { reported <- hub(toHub, outs) }()
	for _, event := range sent {
		toHub <- event
	}
	close(toHub)
	report := <-reported
	running.Wait()
	everyone := slices.Concat(found...)
	slices.SortFunc(everyone, func(a, b Summary) int { return cmp.Compare(a.user, b.user) })
	var received, bytes, mentions, checksum int64
	for _, summary := range everyone {
		received += summary.received
		bytes += summary.bytes
		mentions += summary.mentions
		checksum = (checksum*31 + summary.user*7 + summary.received + hashed(summary.last)) % 1_000_000_007
	}
	fmt.Printf("%d users %d delivered %d bytes %d mentions\n", len(everyone), received, bytes, mentions)
	fmt.Printf("%d refused %d kept in history\n", report.refused, report.history)
	for _, room := range report.rooms {
		fmt.Println(room.name, room.said)
	}
	fmt.Println(everyone[0].last)
	fmt.Println(everyone[len(everyone)-1].last)
	fmt.Println("checksum", checksum)
	if len(os.Args) > 1 {
		fmt.Printf("kernel-ns %d\n", time.Since(started).Nanoseconds())
	}
}
