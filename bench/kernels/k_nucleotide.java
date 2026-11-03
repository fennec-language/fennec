// K-nucleotide, the Java twin of k_nucleotide.fen.
import java.io.OutputStream;
import java.io.PrintStream;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.Map;

public class k_nucleotide {
    static String sequence(int length) {
        long seed = 42;
        char[] alphabet = {'a', 'c', 'g', 't'};
        StringBuilder letters = new StringBuilder(length);
        for (int i = 0; i < length; i++) {
            seed = (seed * 3877 + 29573) % 139968;
            letters.append(alphabet[(int) (seed * 4 / 139968)]);
        }
        return letters.toString();
    }

    static HashMap<String, Long> counted(String dna, int size) {
        HashMap<String, Long> counts = new HashMap<>();
        for (int at = 0; at <= dna.length() - size; at++) {
            counts.merge(dna.substring(at, at + size), 1L, Long::sum);
        }
        return counts;
    }

    static void run(PrintStream out) {
        String dna = sequence(100_000);
        for (int size : new int[] {1, 2}) {
            ArrayList<Map.Entry<String, Long>> ordered = new ArrayList<>(counted(dna, size).entrySet());
            ordered.sort((a, b) -> {
                int byCount = Long.compare(b.getValue(), a.getValue());
                return byCount != 0 ? byCount : a.getKey().compareTo(b.getKey());
            });
            for (Map.Entry<String, Long> entry : ordered) {
                out.println(entry.getKey() + " " + entry.getValue());
            }
        }
        for (String piece : new String[] {"ggt", "ggta", "ggtatt", "ggtattttaatt", "ggtattttaatttatagt"}) {
            out.println(counted(dna, piece.length()).getOrDefault(piece, 0L) + " " + piece);
        }
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
