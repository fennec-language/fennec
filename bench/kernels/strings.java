// Building a long string piece by piece, then scanning it: the Java twin of strings.fen.
public class strings {
    public static void main(String[] args) {
        StringBuilder builder = new StringBuilder();
        for (int i = 0; i < 2_000_000; i++) {
            builder.append(i % 3 == 0 ? "fennec " : "fox ");
        }
        String text = builder.toString();
        long spaces = 0;
        for (int at = 0; at < text.length(); at++) {
            if (text.charAt(at) == ' ') {
                spaces++;
            }
        }
        System.out.println(text.length() + " " + spaces);
    }
}
