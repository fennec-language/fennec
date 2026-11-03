// Comprehensions and iterators, the Java twin of comprehensions.fen.
import java.io.OutputStream;
import java.io.PrintStream;
import java.util.Arrays;
import java.util.HashMap;
import java.util.HashSet;
import java.util.stream.Collectors;
import java.util.stream.IntStream;
import java.util.stream.LongStream;

public class comprehensions {
    static void run(PrintStream out) {
        long size = 100_000;
        long total = 0;
        for (long round = 0; round < 10; round++) {
            final long r = round;
            long[] numbers = LongStream.range(0, size).map(i -> i * 2 + r).toArray();
            long[] evens = Arrays.stream(numbers).filter(n -> n % 4 == 0).toArray();
            long[] squares = Arrays.stream(evens).map(n -> n * n % 1000).toArray();
            total += Arrays.stream(squares).filter(n -> n > 500).sum();
            total += Arrays.stream(squares).map(n -> n % 100).boxed()
                    .collect(Collectors.toCollection(HashSet::new)).size();
            total += Arrays.stream(evens).boxed()
                    .collect(Collectors.toMap(n -> n % 50, n -> n, (a, b) -> b, HashMap::new)).size();
            total += IntStream.range(0, Math.min(numbers.length, squares.length))
                    .mapToLong(at -> numbers[at] * squares[at] % 7).sum();
        }
        out.println(total);
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
