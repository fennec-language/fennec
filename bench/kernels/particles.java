// Structs in a game loop, the Java twin of particles.fen.
import java.io.OutputStream;
import java.io.PrintStream;
import java.util.ArrayList;
import java.util.Locale;

public class particles {
    static final class Vec2 {
        double x;
        double y;

        Vec2(double x, double y) {
            this.x = x;
            this.y = y;
        }
    }

    static final class Particle {
        final Vec2 position;
        final Vec2 velocity;
        long bounces;

        Particle(Vec2 position, Vec2 velocity) {
            this.position = position;
            this.velocity = velocity;
        }

        void step(double dt) {
            velocity.y -= 9.8 * dt;
            position.x += velocity.x * dt;
            position.y += velocity.y * dt;
            if (position.y < 0.0) {
                position.y = -position.y;
                velocity.y = -velocity.y * 0.9;
                bounces++;
            }
        }
    }

    static void run(PrintStream out) {
        int count = 2000;
        ArrayList<Particle> particles = new ArrayList<>();
        for (int i = 0; i < count; i++) {
            particles.add(new Particle(new Vec2(i, i % 50), new Vec2((i % 7) - 3.0, i % 11)));
        }
        for (int round = 0; round < 2500; round++) {
            for (Particle particle : particles) {
                particle.step(0.01);
            }
        }
        long bounces = 0;
        for (Particle particle : particles) {
            bounces += particle.bounces;
        }
        double height = 0.0;
        for (Particle particle : particles) {
            height += particle.position.y;
        }
        out.println(bounces + " " + String.format(Locale.ROOT, "%.6f", height));
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
