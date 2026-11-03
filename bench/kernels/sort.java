// Sorting, the Java twin of sort.fen.
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Collections;
import java.util.Comparator;

public class sort {
    public static void main(String[] args) {
        long seed = 42;
        long[] numbers = new long[300_000];
        for (int i = 0; i < 300_000; i++) {
            seed = (seed * 1103515245L + 12345) % 2147483648L;
            numbers[i] = seed;
        }
        long[] ordered = numbers.clone();
        Arrays.sort(ordered);
        Long[] byDigit = new Long[numbers.length];
        for (int i = 0; i < numbers.length; i++) {
            byDigit[i] = numbers[i];
        }
        Arrays.sort(byDigit, Comparator.comparingLong(n -> n % 1000));
        ArrayList<String> words = new ArrayList<>(50_000);
        for (int i = 0; i < 50_000; i++) {
            words.add("fox" + (numbers[i] % 100000));
        }
        Collections.sort(words);
        System.out.println(ordered[0] + " " + ordered[150_000] + " " + byDigit[0] + " " + byDigit[150_000] + " "
                + words.get(25_000));
    }
}
