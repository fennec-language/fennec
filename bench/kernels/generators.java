// Generators: stages chained through names and through functions, the Java twin of generators.fen.
import java.io.OutputStream;
import java.io.PrintStream;
import java.util.ArrayList;
import java.util.List;
import java.util.NoSuchElementException;
import java.util.PrimitiveIterator;
import java.util.stream.LongStream;
import java.util.stream.Stream;

public class generators {
    record Row(long amount, long weight) {}

    static Row parse(String line) {
        long length = line.codePointCount(0, line.length());
        return new Row(length, length % 7);
    }

    static List<String> makeLines(long count) {
        ArrayList<String> lines = new ArrayList<>();
        for (long at = 0; at < count; at++) {
            if (at % 5 == 0) {
                lines.add("   ");
            } else if (at % 4 == 0) {
                lines.add("  item " + at + " ");
            } else {
                lines.add("item " + at);
            }
        }
        return lines;
    }

    static long pipeline(List<String> src) {
        Stream<String> lines = src.stream().map(String::strip);
        Stream<Row> rows = lines.filter(l -> !l.isEmpty()).map(generators::parse);
        return rows.mapToLong(r -> r.amount() + r.weight()).sum();
    }

    static LongStream countUp(long limit) {
        return LongStream.range(0, limit);
    }

    static LongStream oddSquares(LongStream numbers) {
        return numbers.filter(n -> n % 2 == 1).map(n -> n * n % 1000);
    }

    /** A running total, folded back under 1000 every `size` items. */
    static PrimitiveIterator.OfLong running(PrimitiveIterator.OfLong numbers, long size) {
        return new PrimitiveIterator.OfLong() {
            long total = 0;
            long at = 0;

            @Override
            public boolean hasNext() {
                return numbers.hasNext();
            }

            @Override
            public long nextLong() {
                if (!numbers.hasNext()) {
                    throw new NoSuchElementException();
                }
                total += numbers.nextLong();
                if (at % size == 0) {
                    total %= 1000;
                }
                at++;
                return total;
            }
        };
    }

    static long firstOver(long limit, long over) {
        return oddSquares(countUp(limit)).filter(n -> n > over).findFirst().orElse(0);
    }

    static void run(PrintStream out) {
        List<String> src = makeLines(20_000);
        long result = 0;
        for (long round = 0; round < 40; round++) {
            result += pipeline(src);
            PrimitiveIterator.OfLong chain = running(oddSquares(countUp(200_000 + round)).iterator(), 16);
            long sum = 0;
            while (chain.hasNext()) {
                sum += chain.nextLong() % 7;
            }
            result += sum;
            result += firstOver(1_000, 990 - round);
        }
        out.println(result);
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
