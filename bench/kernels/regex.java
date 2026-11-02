// Regex-redux, the Java twin of regex.fen, on Java's standard java.util.regex engine.
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

    public static void main(String[] args) {
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
            System.out.println(pattern + " " + found);
        }
        String text = dna;
        String[][] changes = {{"tHa[Nt]", "<4>"}, {"aND|caN|Ha[DS]|WaS", "<3>"}, {"a[NSt]|BY", "<2>"}, {"<[^>]*>", "|"}};
        for (String[] change : changes) {
            text = Pattern.compile(change[0]).matcher(text).replaceAll(Matcher.quoteReplacement(change[1]));
        }
        System.out.println(text.length());
        System.out.println(Pattern.compile("ta+c").matcher(dna).replaceAll("x").length());
    }
}
