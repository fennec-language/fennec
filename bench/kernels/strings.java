// Building a long string piece by piece, then scanning it: the Java twin of strings.fen.
import java.io.OutputStream;
import java.io.PrintStream;

public class strings {
    static void run(PrintStream out) {
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
        out.println(text.length() + " " + spaces);
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
