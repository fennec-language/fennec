// The Mandelbrot set, the Java twin of mandelbrot.fen.

public class mandelbrot {
    static long iterations(double cr, double ci, long limit) {
        double zr = 0.0, zi = 0.0;
        for (long i = 0; i < limit; i++) {
            double zr2 = zr * zr, zi2 = zi * zi;
            if (zr2 + zi2 > 4.0) {
                return i;
            }
            zi = 2.0 * zr * zi + ci;
            zr = zr2 - zi2 + cr;
        }
        return limit;
    }

    public static void main(String[] args) {
        int size = 400;
        long total = 0, inside = 0;
        for (int y = 0; y < size; y++) {
            double ci = 2.0 * y / size - 1.0;
            for (int x = 0; x < size; x++) {
                double cr = 2.0 * x / size - 1.5;
                long count = iterations(cr, ci, 100);
                total += count;
                if (count == 100) {
                    inside++;
                }
            }
        }
        System.out.println(total + " " + inside);
    }
}
