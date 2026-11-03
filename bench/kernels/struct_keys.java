// A dict keyed by a plain struct, the Java twin of struct_keys.fen.
import java.io.OutputStream;
import java.io.PrintStream;
import java.util.HashMap;

public class struct_keys {
    record Cell(long x, long y) {}

    static void run(PrintStream out) {
        HashMap<Cell, Long> seen = new HashMap<>();
        for (long i = 0; i < 500_000L; i++) {
            seen.merge(new Cell(i * 31 % 101, i * 17 % 103), 1L, Long::sum);
        }
        out.println(seen.size() + " " + seen.get(new Cell(0, 0)));
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
