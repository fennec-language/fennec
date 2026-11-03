// Regex-redux, the Java twin of regex.fen, on Java's standard java.util.regex engine.
import java.io.OutputStream;
import java.io.PrintStream;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

public class regex {
    static String sequence(int length) {
        long seed = 42;
        StringBuilder letters = new StringBuilder(length);
        for (int i = 0; i < length; i++) {
            seed = (seed * 3877 + 29573) % 139968;
            letters.append("acgt".charAt((int) (seed * 4 / 139968)));
        }
        return letters.toString();
    }

    static void run(PrintStream out) {
        String dna = sequence(50_000);
        String[] patterns = {
            "agggtaaa|tttaccct",
            "[cgt]gggtaaa|tttaccc[acg]",
            "a[act]ggtaaa|tttacc[agt]t",
            "agg[act]taaa|ttta[agt]cct",
            "aggg[acg]aaa|ttt[cgt]ccct",
        };
        for (String pattern : patterns) {
            Matcher matcher = Pattern.compile(pattern).matcher(dna);
            long found = 0;
            while (matcher.find()) {
                found++;
            }
            out.println(pattern + " " + found);
        }
        String text = dna;
        String[][] changes = {{"tHa[Nt]", "<4>"}, {"aND|caN|Ha[DS]|WaS", "<3>"}, {"a[NSt]|BY", "<2>"}, {"<[^>]*>", "|"}};
        for (String[] change : changes) {
            text = Pattern.compile(change[0]).matcher(text).replaceAll(Matcher.quoteReplacement(change[1]));
        }
        out.println(text.length());
        out.println(Pattern.compile("ta+c").matcher(dna).replaceAll("x").length());
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
