//! Richards, the twin of `richards.fen`.

const NONE: i64 = -1;
const IDLE: usize = 0;
const WORKER: i64 = 1;
const HANDLER_A: i64 = 2;
const HANDLER_B: i64 = 3;
const DEVICE_A: i64 = 4;
const DEVICE_B: i64 = 5;
const DEVICE_PACKET: i64 = 0;
const WORK_PACKET: i64 = 1;

#[derive(Clone, Copy)]
enum Kind {
    Idle,
    Worker,
    Handler,
    Device,
}

struct Packet {
    link: i64,
    id: i64,
    kind: i64,
    a1: usize,
    a2: [i64; 4],
}

struct Task {
    kind: Kind,
    priority: i64,
    link: i64,
    queue: i64,
    held: bool,
    suspended: bool,
    runnable: bool,
    v1: i64,
    v2: i64,
}

struct Scheduler {
    tasks: Vec<Task>,
    packets: Vec<Packet>,
    newest: i64,
    current: i64,
    queued: i64,
    holds: i64,
}

impl Scheduler {
    fn add_task(&mut self, kind: Kind, priority: i64, queue: i64, v1: i64, v2: i64) {
        self.tasks.push(Task {
            kind,
            priority,
            link: self.newest,
            queue,
            held: false,
            suspended: true,
            runnable: queue != NONE,
            v1,
            v2,
        });
        self.newest = self.tasks.len() as i64 - 1;
    }

    fn add_packet(&mut self, link: i64, id: i64, kind: i64) -> i64 {
        self.packets.push(Packet { link, id, kind, a1: 0, a2: [0; 4] });
        self.packets.len() as i64 - 1
    }

    fn task(&mut self, at: i64) -> &mut Task {
        &mut self.tasks[at as usize]
    }

    fn packet(&mut self, at: i64) -> &mut Packet {
        &mut self.packets[at as usize]
    }

    fn schedule(&mut self) {
        let mut at = self.newest;
        while at != NONE {
            let task = self.task(at);
            if task.held || (task.suspended && !task.runnable) {
                at = task.link;
            } else {
                self.current = at;
                at = self.run(at);
            }
        }
    }

    fn run(&mut self, at: i64) -> i64 {
        let mut packet = NONE;
        if self.task(at).suspended && self.task(at).runnable {
            packet = self.task(at).queue;
            let next = self.packet(packet).link;
            let task = self.task(at);
            task.queue = next;
            task.suspended = false;
            task.runnable = next != NONE;
        }
        match self.task(at).kind {
            Kind::Idle => self.run_idle(at),
            Kind::Worker => self.run_worker(at, packet),
            Kind::Handler => self.run_handler(at, packet),
            Kind::Device => self.run_device(at, packet),
        }
    }

    fn release(&mut self, id: i64) -> i64 {
        self.task(id).held = false;
        let current = self.current;
        if self.task(id).priority > self.task(current).priority { id } else { current }
    }

    fn hold_current(&mut self) -> i64 {
        self.holds += 1;
        let current = self.current;
        let task = self.task(current);
        task.held = true;
        task.link
    }

    fn suspend_current(&mut self) -> i64 {
        let current = self.current;
        self.task(current).suspended = true;
        current
    }

    fn send(&mut self, packet: i64) -> i64 {
        let to = self.packet(packet).id;
        self.queued += 1;
        let current = self.current;
        let sent = self.packet(packet);
        sent.link = NONE;
        sent.id = current;
        if self.task(to).queue == NONE {
            let priority = self.task(current).priority;
            let task = self.task(to);
            task.queue = packet;
            task.runnable = true;
            if task.priority > priority {
                return to;
            }
        } else {
            let queue = self.task(to).queue;
            self.task(to).queue = self.appended(packet, queue);
        }
        current
    }

    fn appended(&mut self, packet: i64, queue: i64) -> i64 {
        self.packet(packet).link = NONE;
        if queue == NONE {
            return packet;
        }
        let mut last = queue;
        while self.packet(last).link != NONE {
            last = self.packet(last).link;
        }
        self.packet(last).link = packet;
        queue
    }

    fn run_idle(&mut self, at: i64) -> i64 {
        self.task(at).v2 -= 1;
        if self.task(at).v2 == 0 {
            return self.hold_current();
        }
        let state = self.task(at).v1;
        if state & 1 == 0 {
            self.task(at).v1 = state >> 1;
            return self.release(DEVICE_A);
        }
        self.task(at).v1 = (state >> 1) ^ 0xD008;
        self.release(DEVICE_B)
    }

    fn run_device(&mut self, at: i64, packet: i64) -> i64 {
        if packet != NONE {
            self.task(at).v1 = packet;
            return self.hold_current();
        }
        let waiting = self.task(at).v1;
        if waiting == NONE {
            return self.suspend_current();
        }
        self.task(at).v1 = NONE;
        self.send(waiting)
    }

    fn run_worker(&mut self, at: i64, packet: i64) -> i64 {
        if packet == NONE {
            return self.suspend_current();
        }
        let handler = if self.task(at).v1 == HANDLER_A { HANDLER_B } else { HANDLER_A };
        self.task(at).v1 = handler;
        self.packet(packet).id = handler;
        self.packet(packet).a1 = 0;
        for i in 0..4 {
            let mut next = self.task(at).v2 + 1;
            if next > 26 {
                next = 1;
            }
            self.task(at).v2 = next;
            self.packet(packet).a2[i] = next;
        }
        self.send(packet)
    }

    fn run_handler(&mut self, at: i64, packet: i64) -> i64 {
        if packet != NONE {
            if self.packet(packet).kind == WORK_PACKET {
                let queue = self.task(at).v1;
                self.task(at).v1 = self.appended(packet, queue);
            } else {
                let queue = self.task(at).v2;
                self.task(at).v2 = self.appended(packet, queue);
            }
        }
        let work = self.task(at).v1;
        if work != NONE {
            let count = self.packet(work).a1;
            if count < 4 {
                let device = self.task(at).v2;
                if device != NONE {
                    self.task(at).v2 = self.packet(device).link;
                    self.packet(device).a1 = self.packet(work).a2[count] as usize;
                    self.packet(work).a1 = count + 1;
                    return self.send(device);
                }
            } else {
                self.task(at).v1 = self.packet(work).link;
                return self.send(work);
            }
        }
        self.suspend_current()
    }
}

fn simulate() -> (i64, i64) {
    let mut scheduler =
        Scheduler { tasks: Vec::new(), packets: Vec::new(), newest: NONE, current: NONE, queued: 0, holds: 0 };
    scheduler.add_task(Kind::Idle, 0, NONE, 1, std::hint::black_box(10000));
    scheduler.tasks[IDLE].suspended = false;
    let work = scheduler.add_packet(NONE, WORKER, WORK_PACKET);
    let work_queue = scheduler.add_packet(work, WORKER, WORK_PACKET);
    scheduler.add_task(Kind::Worker, 1000, work_queue, HANDLER_A, 0);
    let mut queue = NONE;
    for _ in 0..3 {
        queue = scheduler.add_packet(queue, DEVICE_A, DEVICE_PACKET);
    }
    scheduler.add_task(Kind::Handler, 2000, queue, NONE, NONE);
    queue = NONE;
    for _ in 0..3 {
        queue = scheduler.add_packet(queue, DEVICE_B, DEVICE_PACKET);
    }
    scheduler.add_task(Kind::Handler, 3000, queue, NONE, NONE);
    scheduler.add_task(Kind::Device, 4000, NONE, NONE, 0);
    scheduler.add_task(Kind::Device, 5000, NONE, NONE, 0);
    scheduler.schedule();
    (scheduler.queued, scheduler.holds)
}

fn main() {
    let (mut queued, mut holds) = (0, 0);
    for _ in 0..std::hint::black_box(5) {
        (queued, holds) = simulate();
    }
    println!("{queued} {holds}");
}
