// Counting keys in a hash map, the Java twin of dicts.fen.
import java.util.HashMap;

public class dicts {
    public static void main(String[] args) {
        HashMap<Long, Long> counts = new HashMap<>();
        for (long i = 0; i < 2_000_000L; i++) {
            counts.merge(i * 7919 % 10_007, 1L, Long::sum);
        }
        System.out.println(counts.size() + " " + counts.get(0L) + " " + counts.get(10_006L));
    }
}
