// Binary trees, the Java twin of binary_trees.fen.

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

    public static void main(String[] args) {
        int most = 14;
        Tree longest = build(most);
        long total = 0;
        for (int depth = 4; depth <= most; depth += 4) {
            int rounds = 1 << (most - depth + 4);
            for (int i = 0; i < rounds; i++) {
                total += count(build(depth));
            }
        }
        System.out.println(total + " " + count(longest));
    }
}
