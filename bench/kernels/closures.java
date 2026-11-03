// Closures, the Java twin of closures.fen.
import java.util.function.LongUnaryOperator;

public class closures {
    static long applyAll(long[] values, LongUnaryOperator change) {
        long sum = 0;
        for (long value : values) {
            sum += change.applyAsLong(value);
        }
        return sum;
    }

    static long twice(long value, LongUnaryOperator change) {
        return change.applyAsLong(change.applyAsLong(value));
    }

    public static void main(String[] args) {
        long count = 10_000;
        long[] values = new long[(int) count];
        for (int i = 0; i < count; i++) {
            values[i] = i % 1000;
        }
        long total = 0;
        for (long round = 0; round < 400; round++) {
            long offset = round % 17;
            total += applyAll(values, n -> n + offset);
            total += applyAll(values, n -> twice(n, m -> m * 3 % 1001 + offset));
        }
        System.out.println(total);
    }
}
