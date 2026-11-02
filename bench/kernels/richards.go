// Richards, the Go twin of richards.fen.
package main

import "fmt"

const (
	none         int64 = -1
	idle               = 0
	worker       int64 = 1
	handlerA     int64 = 2
	handlerB     int64 = 3
	deviceA      int64 = 4
	deviceB      int64 = 5
	devicePacket int64 = 0
	workPacket   int64 = 1
)

type kind int

const (
	kindIdle kind = iota
	kindWorker
	kindHandler
	kindDevice
)

type packet struct {
	link int64
	id   int64
	kind int64
	a1   int
	a2   [4]int64
}

type task struct {
	kind      kind
	priority  int64
	link      int64
	queue     int64
	held      bool
	suspended bool
	runnable  bool
	v1        int64
	v2        int64
}

type scheduler struct {
	tasks   []task
	packets []packet
	newest  int64
	current int64
	queued  int64
	holds   int64
}

func (s *scheduler) addTask(k kind, priority, queue, v1, v2 int64) {
	s.tasks = append(s.tasks, task{
		kind:      k,
		priority:  priority,
		link:      s.newest,
		queue:     queue,
		suspended: true,
		runnable:  queue != none,
		v1:        v1,
		v2:        v2,
	})
	s.newest = int64(len(s.tasks)) - 1
}

func (s *scheduler) addPacket(link, id, k int64) int64 {
	s.packets = append(s.packets, packet{link: link, id: id, kind: k})
	return int64(len(s.packets)) - 1
}

func (s *scheduler) task(at int64) *task     { return &s.tasks[at] }
func (s *scheduler) packet(at int64) *packet { return &s.packets[at] }

func (s *scheduler) schedule() {
	at := s.newest
	for at != none {
		t := s.task(at)
		if t.held || (t.suspended && !t.runnable) {
			at = t.link
		} else {
			s.current = at
			at = s.run(at)
		}
	}
}

func (s *scheduler) run(at int64) int64 {
	p := none
	if s.task(at).suspended && s.task(at).runnable {
		p = s.task(at).queue
		next := s.packet(p).link
		t := s.task(at)
		t.queue = next
		t.suspended = false
		t.runnable = next != none
	}
	switch s.task(at).kind {
	case kindIdle:
		return s.runIdle(at)
	case kindWorker:
		return s.runWorker(at, p)
	case kindHandler:
		return s.runHandler(at, p)
	default:
		return s.runDevice(at, p)
	}
}

func (s *scheduler) release(id int64) int64 {
	s.task(id).held = false
	if s.task(id).priority > s.task(s.current).priority {
		return id
	}
	return s.current
}

func (s *scheduler) holdCurrent() int64 {
	s.holds++
	t := s.task(s.current)
	t.held = true
	return t.link
}

func (s *scheduler) suspendCurrent() int64 {
	s.task(s.current).suspended = true
	return s.current
}

func (s *scheduler) send(p int64) int64 {
	to := s.packet(p).id
	s.queued++
	current := s.current
	sent := s.packet(p)
	sent.link = none
	sent.id = current
	if s.task(to).queue == none {
		priority := s.task(current).priority
		t := s.task(to)
		t.queue = p
		t.runnable = true
		if t.priority > priority {
			return to
		}
	} else {
		queue := s.task(to).queue
		s.task(to).queue = s.appended(p, queue)
	}
	return current
}

func (s *scheduler) appended(p, queue int64) int64 {
	s.packet(p).link = none
	if queue == none {
		return p
	}
	last := queue
	for s.packet(last).link != none {
		last = s.packet(last).link
	}
	s.packet(last).link = p
	return queue
}

func (s *scheduler) runIdle(at int64) int64 {
	s.task(at).v2--
	if s.task(at).v2 == 0 {
		return s.holdCurrent()
	}
	state := s.task(at).v1
	if state&1 == 0 {
		s.task(at).v1 = state >> 1
		return s.release(deviceA)
	}
	s.task(at).v1 = (state >> 1) ^ 0xD008
	return s.release(deviceB)
}

func (s *scheduler) runDevice(at, p int64) int64 {
	if p != none {
		s.task(at).v1 = p
		return s.holdCurrent()
	}
	waiting := s.task(at).v1
	if waiting == none {
		return s.suspendCurrent()
	}
	s.task(at).v1 = none
	return s.send(waiting)
}

func (s *scheduler) runWorker(at, p int64) int64 {
	if p == none {
		return s.suspendCurrent()
	}
	handler := handlerA
	if s.task(at).v1 == handlerA {
		handler = handlerB
	}
	s.task(at).v1 = handler
	s.packet(p).id = handler
	s.packet(p).a1 = 0
	for i := 0; i < 4; i++ {
		next := s.task(at).v2 + 1
		if next > 26 {
			next = 1
		}
		s.task(at).v2 = next
		s.packet(p).a2[i] = next
	}
	return s.send(p)
}

func (s *scheduler) runHandler(at, p int64) int64 {
	if p != none {
		if s.packet(p).kind == workPacket {
			queue := s.task(at).v1
			s.task(at).v1 = s.appended(p, queue)
		} else {
			queue := s.task(at).v2
			s.task(at).v2 = s.appended(p, queue)
		}
	}
	work := s.task(at).v1
	if work != none {
		count := s.packet(work).a1
		if count < 4 {
			device := s.task(at).v2
			if device != none {
				s.task(at).v2 = s.packet(device).link
				s.packet(device).a1 = int(s.packet(work).a2[count])
				s.packet(work).a1 = count + 1
				return s.send(device)
			}
		} else {
			s.task(at).v1 = s.packet(work).link
			return s.send(work)
		}
	}
	return s.suspendCurrent()
}

func simulate() (int64, int64) {
	s := &scheduler{newest: none, current: none}
	s.addTask(kindIdle, 0, none, 1, 10000)
	s.tasks[idle].suspended = false
	work := s.addPacket(none, worker, workPacket)
	workQueue := s.addPacket(work, worker, workPacket)
	s.addTask(kindWorker, 1000, workQueue, handlerA, 0)
	queue := none
	for i := 0; i < 3; i++ {
		queue = s.addPacket(queue, deviceA, devicePacket)
	}
	s.addTask(kindHandler, 2000, queue, none, none)
	queue = none
	for i := 0; i < 3; i++ {
		queue = s.addPacket(queue, deviceB, devicePacket)
	}
	s.addTask(kindHandler, 3000, queue, none, none)
	s.addTask(kindDevice, 4000, none, none, 0)
	s.addTask(kindDevice, 5000, none, none, 0)
	s.schedule()
	return s.queued, s.holds
}

func main() {
	var queued, holds int64
	for i := 0; i < 25; i++ {
		queued, holds = simulate()
	}
	fmt.Printf("%d %d\n", queued, holds)
}
