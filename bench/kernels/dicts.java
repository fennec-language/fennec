// Counting keys in a hash map, the Java twin of dicts.fen.
import java.io.OutputStream;
import java.io.PrintStream;
import java.util.HashMap;

public class dicts {
    static void run(PrintStream out) {
        HashMap<Long, Long> counts = new HashMap<>();
        for (long i = 0; i < 2_000_000L; i++) {
            counts.merge(i * 7919 % 10_007, 1L, Long::sum);
        }
        out.println(counts.size() + " " + counts.get(0L) + " " + counts.get(10_006L));
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
