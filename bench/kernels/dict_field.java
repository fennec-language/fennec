// A dict in a struct's field, the Java twin of dict_field.fen.
import java.util.HashMap;

public class dict_field {
    static final class Counter {
        final HashMap<Long, Long> counts = new HashMap<>();

        void bump(long key) {
            counts.merge(key, 1L, Long::sum);
        }
    }

    public static void main(String[] args) {
        Counter counter = new Counter();
        for (long i = 0; i < 2_500_000L; i++) {
            counter.bump(i * 7919 % 64);
        }
        System.out.println(counter.counts.size() + " " + counter.counts.get(0L) + " " + counter.counts.get(63L));
    }
}
