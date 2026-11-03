// A dict keyed by a plain struct, the Java twin of struct_keys.fen.
import java.util.HashMap;

public class struct_keys {
    record Cell(long x, long y) {}

    public static void main(String[] args) {
        HashMap<Cell, Long> seen = new HashMap<>();
        for (long i = 0; i < 500_000L; i++) {
            seen.merge(new Cell(i * 31 % 101, i * 17 % 103), 1L, Long::sum);
        }
        System.out.println(seen.size() + " " + seen.get(new Cell(0, 0)));
    }
}
