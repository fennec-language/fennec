// Chained method calls on small structs, the Java twin of method_chain.fen.
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

    public static void main(String[] args) {
        int size = 500;
        long total = 0;
        for (int y = 0; y < size; y++) {
            double ci = 2.0 * y / size - 1.0;
            for (int x = 0; x < size; x++) {
                double cr = 2.0 * x / size - 1.5;
                total += iterations(new Complex(cr, ci), 100);
            }
        }
        System.out.println(total);
    }
}
