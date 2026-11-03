// Chained method calls on small structs, the Java twin of method_chain.fen.
import java.io.OutputStream;
import java.io.PrintStream;

public class method_chain {
    record Complex(double re, double im) {
        Complex plus(Complex other) {
            return new Complex(re + other.re, im + other.im);
        }

        Complex times(Complex other) {
            return new Complex(re * other.re - im * other.im, re * other.im + im * other.re);
        }

        double norm() {
            return re * re + im * im;
        }
    }

    static long iterations(Complex c, long limit) {
        Complex z = new Complex(0.0, 0.0);
        for (long i = 0; i < limit; i++) {
            if (z.norm() > 4.0) {
                return i;
            }
            z = z.times(z).plus(c);
        }
        return limit;
    }

    static void run(PrintStream out) {
        int size = 500;
        long total = 0;
        for (int y = 0; y < size; y++) {
            double ci = 2.0 * y / size - 1.0;
            for (int x = 0; x < size; x++) {
                double cr = 2.0 * x / size - 1.5;
                total += iterations(new Complex(cr, ci), 100);
            }
        }
        out.println(total);
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
