// Binary trees, the Java twin of binary_trees.fen.

import java.io.OutputStream;
import java.io.PrintStream;

public class binary_trees {
    // A leaf when both children are null, and a node otherwise.
    static final class Tree {
        final Tree left;
        final Tree right;

        Tree(Tree left, Tree right) {
            this.left = left;
            this.right = right;
        }
    }

    static Tree build(int depth) {
        if (depth == 0) {
            return new Tree(null, null);
        }
        return new Tree(build(depth - 1), build(depth - 1));
    }

    static long count(Tree tree) {
        if (tree.left == null) {
            return 1;
        }
        return 1 + count(tree.left) + count(tree.right);
    }

    static void run(PrintStream out) {
        int most = 14;
        Tree longest = build(most);
        long total = 0;
        for (int depth = 4; depth <= most; depth += 4) {
            int rounds = 1 << (most - depth + 4);
            for (int i = 0; i < rounds; i++) {
                total += count(build(depth));
            }
        }
        out.println(total + " " + count(longest));
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
