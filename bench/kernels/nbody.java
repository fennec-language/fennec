// The n-body simulation from the Benchmarks Game, the Java twin of nbody.fen.

import java.io.OutputStream;
import java.io.PrintStream;
import java.math.BigDecimal;
import java.math.RoundingMode;

public class nbody {
    static final class Body {
        double x, y, z, vx, vy, vz, mass;

        Body(double x, double y, double z, double vx, double vy, double vz, double mass) {
            this.x = x;
            this.y = y;
            this.z = z;
            this.vx = vx;
            this.vy = vy;
            this.vz = vz;
            this.mass = mass;
        }
    }

    static final double PI = Math.PI;
    static final double SOLAR_MASS = 4.0 * PI * PI;
    static final double DAYS = 365.24;

    static Body planet(double x, double y, double z, double vx, double vy, double vz, double mass) {
        return new Body(x, y, z, vx * DAYS, vy * DAYS, vz * DAYS, mass * SOLAR_MASS);
    }

    static double energy(Body[] bodies) {
        double total = 0.0;
        for (int i = 0; i < bodies.length; i++) {
            Body a = bodies[i];
            total += 0.5 * a.mass * (a.vx * a.vx + a.vy * a.vy + a.vz * a.vz);
            for (int j = i + 1; j < bodies.length; j++) {
                Body b = bodies[j];
                double dx = a.x - b.x, dy = a.y - b.y, dz = a.z - b.z;
                total -= a.mass * b.mass / Math.sqrt(dx * dx + dy * dy + dz * dz);
            }
        }
        return total;
    }

    static void advance(Body[] bodies, double dt) {
        int count = bodies.length;
        for (int i = 0; i < count; i++) {
            for (int j = i + 1; j < count; j++) {
                double dx = bodies[i].x - bodies[j].x, dy = bodies[i].y - bodies[j].y, dz = bodies[i].z - bodies[j].z;
                double squared = dx * dx + dy * dy + dz * dz;
                double magnitude = dt / (squared * Math.sqrt(squared));
                double massI = bodies[i].mass, massJ = bodies[j].mass;
                bodies[i].vx -= dx * massJ * magnitude;
                bodies[i].vy -= dy * massJ * magnitude;
                bodies[i].vz -= dz * massJ * magnitude;
                bodies[j].vx += dx * massI * magnitude;
                bodies[j].vy += dy * massI * magnitude;
                bodies[j].vz += dz * massI * magnitude;
            }
        }
        for (Body body : bodies) {
            body.x += dt * body.vx;
            body.y += dt * body.vy;
            body.z += dt * body.vz;
        }
    }

    // Exact decimal rounding, as Rust's {:.9} does.
    static String fixed9(double value) {
        return new BigDecimal(value).setScale(9, RoundingMode.HALF_EVEN).toPlainString();
    }

    static void run(PrintStream out) {
        Body[] bodies = {
            new Body(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, SOLAR_MASS),
            planet(4.84143144246472090e+00, -1.16032004402742839e+00, -1.03622044471123109e-01, 1.66007664274403694e-03, 7.69901118419740425e-03, -6.90460016972063023e-05, 9.54791938424326609e-04),
            planet(8.34336671824457987e+00, 4.12479856412430479e+00, -4.03523417114321381e-01, -2.76742510726862411e-03, 4.99852801234917238e-03, 2.30417297573763929e-05, 2.85885980666130812e-04),
            planet(1.28943695621391310e+01, -1.51111514016986312e+01, -2.23307578892655734e-01, 2.96460137564761618e-03, 2.37847173959480950e-03, -2.96589568540237556e-05, 4.36624404335156298e-05),
            planet(1.53796971148509165e+01, -2.59193146099879641e+01, 1.79258772950371181e-01, 2.68067772490389322e-03, 1.62824170038242295e-03, -9.51592254519715870e-05, 5.15138902046611451e-05),
        };
        double px = 0.0, py = 0.0, pz = 0.0;
        for (Body body : bodies) {
            px += body.vx * body.mass;
            py += body.vy * body.mass;
            pz += body.vz * body.mass;
        }
        bodies[0].vx = -px / SOLAR_MASS;
        bodies[0].vy = -py / SOLAR_MASS;
        bodies[0].vz = -pz / SOLAR_MASS;
        out.println(fixed9(energy(bodies)));
        for (int step = 0; step < 200_000; step++) {
            advance(bodies, 0.01);
        }
        out.println(fixed9(energy(bodies)));
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
