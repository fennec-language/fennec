// Generic code over an interface, the Java twin of generics.fen.
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;
import java.util.function.BiPredicate;

public class generics {
    interface Shape {
        double area();

        double scaled(double by);
    }

    record Circle(double radius) implements Shape {
        public double area() {
            return 3.14159 * radius * radius;
        }

        public double scaled(double by) {
            return area() * by * by;
        }
    }

    record Square(double side) implements Shape {
        public double area() {
            return side * side;
        }

        public double scaled(double by) {
            return area() * by * by;
        }
    }

    static <T extends Shape> double total(List<T> shapes, double by) {
        double sum = 0.0;
        for (T shape : shapes) {
            sum += shape.area() + shape.scaled(by);
        }
        return sum;
    }

    static <T> T largest(List<T> items, BiPredicate<T, T> better) {
        T best = items.get(0);
        for (T item : items) {
            if (better.test(item, best)) {
                best = item;
            }
        }
        return best;
    }

    public static void main(String[] args) {
        int count = 5000;
        ArrayList<Circle> circles = new ArrayList<>();
        ArrayList<Square> squares = new ArrayList<>();
        for (int i = 0; i < count; i++) {
            circles.add(new Circle(i % 10));
        }
        for (int i = 0; i < count; i++) {
            squares.add(new Square(i % 13));
        }
        double sum = 0.0;
        for (int round = 0; round < 2500; round++) {
            double by = (round % 3) + 1.0;
            sum += total(circles, by) + total(squares, by);
        }
        Circle biggest = largest(circles, (a, b) -> a.radius() > b.radius());
        System.out.println(String.format(Locale.ROOT, "%.3f", sum) + " " + biggest.radius());
    }
}
