// The sieve of Eratosthenes, the Java twin of sieve.fen.
import java.io.OutputStream;
import java.io.PrintStream;

public class sieve {
    static long countPrimes(int limit) {
        long[] composite = new long[limit + 1];
        long count = 0;
        for (int n = 2; n <= limit; n++) {
            if (composite[n] == 0) {
                count += 1;
                long multiple = (long) n * n;
                while (multiple <= limit) {
                    composite[(int) multiple] = 1;
                    multiple += n;
                }
            }
        }
        return count;
    }

    static void run(PrintStream out) {
        out.println(countPrimes(5_000_000));
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
