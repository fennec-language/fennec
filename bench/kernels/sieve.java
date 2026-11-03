// The sieve of Eratosthenes, the Java twin of sieve.fen.
public class sieve {
    static long countPrimes(int limit) {
        long[] composite = new long[limit + 1];
        long count = 0;
        for (int n = 2; n <= limit; n++) {
            if (composite[n] == 0) {
                count += 1;
                long multiple = (long) n * n;
                while (multiple <= limit) {
                    composite[(int) multiple] = 1;
                    multiple += n;
                }
            }
        }
        return count;
    }

    public static void main(String[] args) {
        System.out.println(countPrimes(5_000_000));
    }
}
