// The spectral norm from the Benchmarks Game, the Java twin of spectral_norm.fen.

import java.io.OutputStream;
import java.io.PrintStream;
import java.math.BigDecimal;
import java.math.RoundingMode;
import java.util.Arrays;

public class spectral_norm {
    static double a(long i, long j) {
        return 1.0 / ((i + j) * (i + j + 1) / 2 + i + 1);
    }

    static double[] times(double[] v, boolean transpose) {
        double[] result = new double[v.length];
        for (int i = 0; i < v.length; i++) {
            double sum = 0.0;
            for (int j = 0; j < v.length; j++) {
                sum += (transpose ? a(j, i) : a(i, j)) * v[j];
            }
            result[i] = sum;
        }
        return result;
    }

    static double[] timesTransposed(double[] v) {
        return times(times(v, false), true);
    }

    // Exact decimal rounding, as Rust's {:.9} does.
    static String fixed9(double value) {
        return new BigDecimal(value).setScale(9, RoundingMode.HALF_EVEN).toPlainString();
    }

    static void run(PrintStream out) {
        int n = 400;
        double[] u = new double[n];
        Arrays.fill(u, 1.0);
        double[] v = new double[n];
        for (int round = 0; round < 10; round++) {
            v = timesTransposed(u);
            u = timesTransposed(v);
        }
        double vbv = 0.0, vv = 0.0;
        for (int i = 0; i < n; i++) {
            vbv += u[i] * v[i];
        }
        for (double x : v) {
            vv += x * x;
        }
        out.println(fixed9(Math.sqrt(vbv / vv)));
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
