// Richards, the Java twin of richards.fen.
import java.io.OutputStream;
import java.io.PrintStream;
import java.util.ArrayList;

public class richards {
    static final long NONE = -1;
    static final int IDLE = 0;
    static final long WORKER = 1;
    static final long HANDLER_A = 2;
    static final long HANDLER_B = 3;
    static final long DEVICE_A = 4;
    static final long DEVICE_B = 5;
    static final long DEVICE_PACKET = 0;
    static final long WORK_PACKET = 1;

    enum Kind { IDLE, WORKER, HANDLER, DEVICE }

    static final class Packet {
        long link;
        long id;
        long kind;
        int a1;
        final long[] a2 = new long[4];

        Packet(long link, long id, long kind) {
            this.link = link;
            this.id = id;
            this.kind = kind;
        }
    }

    static final class Task {
        final Kind kind;
        final long priority;
        final long link;
        long queue;
        boolean held;
        boolean suspended;
        boolean runnable;
        long v1;
        long v2;

        Task(Kind kind, long priority, long link, long queue, long v1, long v2) {
            this.kind = kind;
            this.priority = priority;
            this.link = link;
            this.queue = queue;
            this.held = false;
            this.suspended = true;
            this.runnable = queue != NONE;
            this.v1 = v1;
            this.v2 = v2;
        }
    }

    static final class Scheduler {
        final ArrayList<Task> tasks = new ArrayList<>();
        final ArrayList<Packet> packets = new ArrayList<>();
        long newest = NONE;
        long current = NONE;
        long queued = 0;
        long holds = 0;

        void addTask(Kind kind, long priority, long queue, long v1, long v2) {
            tasks.add(new Task(kind, priority, newest, queue, v1, v2));
            newest = tasks.size() - 1;
        }

        long addPacket(long link, long id, long kind) {
            packets.add(new Packet(link, id, kind));
            return packets.size() - 1;
        }

        Task task(long at) {
            return tasks.get((int) at);
        }

        Packet packet(long at) {
            return packets.get((int) at);
        }

        void schedule() {
            long at = newest;
            while (at != NONE) {
                Task task = task(at);
                if (task.held || (task.suspended && !task.runnable)) {
                    at = task.link;
                } else {
                    current = at;
                    at = run(at);
                }
            }
        }

        long run(long at) {
            long packet = NONE;
            if (task(at).suspended && task(at).runnable) {
                packet = task(at).queue;
                long next = packet(packet).link;
                Task task = task(at);
                task.queue = next;
                task.suspended = false;
                task.runnable = next != NONE;
            }
            switch (task(at).kind) {
                case IDLE: return runIdle(at);
                case WORKER: return runWorker(at, packet);
                case HANDLER: return runHandler(at, packet);
                default: return runDevice(at, packet);
            }
        }

        long release(long id) {
            task(id).held = false;
            return task(id).priority > task(current).priority ? id : current;
        }

        long holdCurrent() {
            holds += 1;
            Task task = task(current);
            task.held = true;
            return task.link;
        }

        long suspendCurrent() {
            task(current).suspended = true;
            return current;
        }

        long send(long packet) {
            long to = packet(packet).id;
            queued += 1;
            Packet sent = packet(packet);
            sent.link = NONE;
            sent.id = current;
            if (task(to).queue == NONE) {
                long priority = task(current).priority;
                Task task = task(to);
                task.queue = packet;
                task.runnable = true;
                if (task.priority > priority) {
                    return to;
                }
            } else {
                long queue = task(to).queue;
                task(to).queue = appended(packet, queue);
            }
            return current;
        }

        long appended(long packet, long queue) {
            packet(packet).link = NONE;
            if (queue == NONE) {
                return packet;
            }
            long last = queue;
            while (packet(last).link != NONE) {
                last = packet(last).link;
            }
            packet(last).link = packet;
            return queue;
        }

        long runIdle(long at) {
            task(at).v2 -= 1;
            if (task(at).v2 == 0) {
                return holdCurrent();
            }
            long state = task(at).v1;
            if ((state & 1) == 0) {
                task(at).v1 = state >> 1;
                return release(DEVICE_A);
            }
            task(at).v1 = (state >> 1) ^ 0xD008;
            return release(DEVICE_B);
        }

        long runDevice(long at, long packet) {
            if (packet != NONE) {
                task(at).v1 = packet;
                return holdCurrent();
            }
            long waiting = task(at).v1;
            if (waiting == NONE) {
                return suspendCurrent();
            }
            task(at).v1 = NONE;
            return send(waiting);
        }

        long runWorker(long at, long packet) {
            if (packet == NONE) {
                return suspendCurrent();
            }
            long handler = task(at).v1 == HANDLER_A ? HANDLER_B : HANDLER_A;
            task(at).v1 = handler;
            packet(packet).id = handler;
            packet(packet).a1 = 0;
            for (int i = 0; i < 4; i++) {
                long next = task(at).v2 + 1;
                if (next > 26) {
                    next = 1;
                }
                task(at).v2 = next;
                packet(packet).a2[i] = next;
            }
            return send(packet);
        }

        long runHandler(long at, long packet) {
            if (packet != NONE) {
                if (packet(packet).kind == WORK_PACKET) {
                    long queue = task(at).v1;
                    task(at).v1 = appended(packet, queue);
                } else {
                    long queue = task(at).v2;
                    task(at).v2 = appended(packet, queue);
                }
            }
            long work = task(at).v1;
            if (work != NONE) {
                int count = packet(work).a1;
                if (count < 4) {
                    long device = task(at).v2;
                    if (device != NONE) {
                        task(at).v2 = packet(device).link;
                        packet(device).a1 = (int) packet(work).a2[count];
                        packet(work).a1 = count + 1;
                        return send(device);
                    }
                } else {
                    task(at).v1 = packet(work).link;
                    return send(work);
                }
            }
            return suspendCurrent();
        }
    }

    static long[] simulate() {
        Scheduler scheduler = new Scheduler();
        scheduler.addTask(Kind.IDLE, 0, NONE, 1, 10000);
        scheduler.tasks.get(IDLE).suspended = false;
        long work = scheduler.addPacket(NONE, WORKER, WORK_PACKET);
        long workQueue = scheduler.addPacket(work, WORKER, WORK_PACKET);
        scheduler.addTask(Kind.WORKER, 1000, workQueue, HANDLER_A, 0);
        long queue = NONE;
        for (int i = 0; i < 3; i++) {
            queue = scheduler.addPacket(queue, DEVICE_A, DEVICE_PACKET);
        }
        scheduler.addTask(Kind.HANDLER, 2000, queue, NONE, NONE);
        queue = NONE;
        for (int i = 0; i < 3; i++) {
            queue = scheduler.addPacket(queue, DEVICE_B, DEVICE_PACKET);
        }
        scheduler.addTask(Kind.HANDLER, 3000, queue, NONE, NONE);
        scheduler.addTask(Kind.DEVICE, 4000, NONE, NONE, 0);
        scheduler.addTask(Kind.DEVICE, 5000, NONE, NONE, 0);
        scheduler.schedule();
        return new long[] {scheduler.queued, scheduler.holds};
    }

    static void run(PrintStream out) {
        long queued = 0;
        long holds = 0;
        for (int i = 0; i < 25; i++) {
            long[] result = simulate();
            queued = result[0];
            holds = result[1];
        }
        out.println(queued + " " + holds);
    }

    public static void main(String[] args) {
        if (args.length == 0) {
            run(System.out);
            return;
        }
        run(new PrintStream(OutputStream.nullOutputStream()));
        long started = System.nanoTime();
        run(System.out);
        System.out.flush();
        System.out.println("kernel-ns " + (System.nanoTime() - started));
    }
}
