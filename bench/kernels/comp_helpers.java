// Iterators that call helper functions, the Java twin of comp_helpers.fen.
import java.util.Arrays;
import java.util.Locale;

public class comp_helpers {
    static double a(int i, int j) {
        return 1.0 / ((i + j) * (i + j + 1) / 2 + i + 1);
    }

    static double row(double[] v, int i, boolean transpose) {
        double sum = 0.0;
        for (int j = 0; j < v.length; j++) {
            sum += (transpose ? a(j, i) : a(i, j)) * v[j];
        }
        return sum;
    }

    static double[] times(double[] v, boolean transpose) {
        double[] out = new double[v.length];
        for (int i = 0; i < v.length; i++) {
            out[i] = row(v, i, transpose);
        }
        return out;
    }

    static double[] timesTransposed(double[] v) {
        return times(times(v, false), true);
    }

    public static void main(String[] args) {
        int n = 100;
        double[] u = new double[n];
        Arrays.fill(u, 1.0);
        double[] v = new double[n];
        for (int round = 0; round < 300; round++) {
            v = timesTransposed(u);
            u = timesTransposed(v);
        }
        double vbv = 0.0;
        for (int i = 0; i < n; i++) {
            vbv += u[i] * v[i];
        }
        double vv = 0.0;
        for (double x : v) {
            vv += x * x;
        }
        System.out.println(String.format(Locale.ROOT, "%.9f", Math.sqrt(vbv / vv)));
    }
}
