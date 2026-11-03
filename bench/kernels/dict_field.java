// A dict in a struct's field, the Java twin of dict_field.fen.
import java.io.OutputStream;
import java.io.PrintStream;
import java.util.HashMap;

public class dict_field {
    static final class Counter {
        final HashMap<Long, Long> counts = new HashMap<>();

        void bump(long key) {
            counts.merge(key, 1L, Long::sum);
        }
    }

    static void run(PrintStream out) {
        Counter counter = new Counter();
        for (long i = 0; i < 2_500_000L; i++) {
            counter.bump(i * 7919 % 64);
        }
        out.println(counter.counts.size() + " " + counter.counts.get(0L) + " " + counter.counts.get(63L));
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
