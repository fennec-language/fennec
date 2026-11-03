// Fannkuch, the Java twin of fannkuch.fen.
public class fannkuch {
    static long[] fannkuch(int n) {
        int[] perm = new int[n];
        for (int i = 0; i < n; i++) {
            perm[i] = i;
        }
        int[] count = new int[n];
        int[] flips = new int[n];
        long most = 0, checksum = 0, sign = 1;
        while (true) {
            System.arraycopy(perm, 0, flips, 0, n);
            long total = 0;
            int first = flips[0];
            while (first != 0) {
                for (int lo = 0, hi = first; lo < hi; lo++, hi--) {
                    int t = flips[lo];
                    flips[lo] = flips[hi];
                    flips[hi] = t;
                }
                total++;
                first = flips[0];
            }
            most = Math.max(most, total);
            checksum += sign * total;
            sign = -sign;
            int r = 1;
            while (true) {
                if (r == n) {
                    return new long[] {checksum, most};
                }
                int head = perm[0];
                System.arraycopy(perm, 1, perm, 0, r);
                perm[r] = head;
                count[r]++;
                if (count[r] <= r) {
                    break;
                }
                count[r] = 0;
                r++;
            }
        }
    }

    public static void main(String[] args) {
        long[] result = fannkuch(9);
        System.out.println(result[0] + " " + result[1]);
    }
}
