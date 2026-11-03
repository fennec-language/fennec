// DeltaBlue, the Java twin of deltablue.fen.
import java.io.OutputStream;
import java.io.PrintStream;
import java.util.ArrayList;

public class deltablue {
    static final long NONE = -1;

    static final long REQUIRED = 0;
    static final long PREFERRED = 2;
    static final long STRONG_DEFAULT = 3;
    static final long NORMAL = 4;
    static final long WEAKEST = 6;

    static final long UNDECIDED = 0;
    static final long FORWARD = 1;
    static final long BACKWARD = 2;

    enum Kind { STAY, EDIT, EQUAL, SCALE }

    static final class Variable {
        long value;
        ArrayList<Long> constraints = new ArrayList<>();
        long determinedBy = NONE;
        long mark = 0;
        long walk = WEAKEST;
        boolean stay = true;

        Variable(long value) {
            this.value = value;
        }
    }

    static final class Constraint {
        final Kind kind;
        final long strength;
        final long first;
        final long second;
        long scale = NONE;
        long offset = NONE;
        long direction = UNDECIDED;

        Constraint(Kind kind, long strength, long first, long second) {
            this.kind = kind;
            this.strength = strength;
            this.first = first;
            this.second = second;
        }

        boolean unary() {
            return kind == Kind.STAY || kind == Kind.EDIT;
        }

        boolean satisfied() {
            return direction != UNDECIDED;
        }

        long output() {
            return unary() || direction == BACKWARD ? first : second;
        }

        long input() {
            return direction == BACKWARD ? second : first;
        }
    }

    static final class Planner {
        final ArrayList<Variable> variables = new ArrayList<>();
        final ArrayList<Constraint> constraints = new ArrayList<>();
        long currentMark = 0;

        long variable(long value) {
            variables.add(new Variable(value));
            return variables.size() - 1;
        }

        Variable var(long at) {
            return variables.get((int) at);
        }

        Constraint con(long at) {
            return constraints.get((int) at);
        }

        void setDirection(long at, long direction) {
            constraints.get((int) at).direction = direction;
        }

        long newMark() {
            currentMark += 1;
            return currentMark;
        }

        long add(Constraint constraint) {
            constraints.add(constraint);
            long at = constraints.size() - 1;
            addToGraph(at);
            incrementalAdd(at);
            return at;
        }

        void addToGraph(long at) {
            Constraint c = con(at);
            var(c.first).constraints.add(at);
            if (!c.unary()) {
                var(c.second).constraints.add(at);
            }
            if (c.scale != NONE) {
                var(c.scale).constraints.add(at);
                var(c.offset).constraints.add(at);
            }
            setDirection(at, UNDECIDED);
        }

        void removeFromGraph(long at) {
            Constraint c = con(at);
            for (long place : new long[] {c.first, c.second, c.scale, c.offset}) {
                if (place != NONE) {
                    Variable variable = var(place);
                    variable.constraints.removeIf(other -> other == at);
                    if (variable.determinedBy == at) {
                        variable.determinedBy = NONE;
                    }
                }
            }
            setDirection(at, UNDECIDED);
        }

        void chooseMethod(long at, long mark) {
            Constraint c = con(at);
            if (c.unary()) {
                Variable first = var(c.first);
                boolean taken = first.mark != mark && c.strength < first.walk;
                setDirection(at, taken ? FORWARD : UNDECIDED);
                return;
            }
            long firstMark = var(c.first).mark, firstWalk = var(c.first).walk;
            long secondMark = var(c.second).mark, secondWalk = var(c.second).walk;
            long direction = UNDECIDED;
            if (firstMark == mark) {
                if (secondMark != mark && c.strength < secondWalk) {
                    direction = FORWARD;
                }
            } else if (secondMark == mark) {
                if (firstMark != mark && c.strength < firstWalk) {
                    direction = BACKWARD;
                }
            } else if (firstWalk > secondWalk) {
                if (c.strength < firstWalk) {
                    direction = BACKWARD;
                }
            } else if (c.strength < secondWalk) {
                direction = FORWARD;
            }
            setDirection(at, direction);
        }

        void markInputs(long at, long mark) {
            Constraint c = con(at);
            if (c.unary()) {
                return;
            }
            var(c.input()).mark = mark;
            if (c.scale != NONE) {
                var(c.scale).mark = mark;
                var(c.offset).mark = mark;
            }
        }

        boolean inputsKnown(long at, long mark) {
            Constraint c = con(at);
            if (c.unary()) {
                return true;
            }
            Variable input = var(c.input());
            return input.mark == mark || input.stay || input.determinedBy == NONE;
        }

        void recalculate(long at) {
            Constraint c = con(at);
            long out = c.output();
            if (c.unary()) {
                Variable output = var(out);
                output.walk = c.strength;
                output.stay = c.kind != Kind.EDIT;
                return;
            }
            long input = c.input();
            long walk = Math.max(c.strength, var(input).walk);
            boolean stay = var(input).stay;
            if (c.scale != NONE) {
                stay = stay && var(c.scale).stay && var(c.offset).stay;
            }
            Variable output = var(out);
            output.walk = walk;
            output.stay = stay;
            if (stay) {
                execute(at);
            }
        }

        static long divEuclid(long a, long b) {
            long q = a / b;
            if (a % b < 0) {
                q = b > 0 ? q - 1 : q + 1;
            }
            return q;
        }

        void execute(long at) {
            Constraint c = con(at);
            switch (c.kind) {
                case STAY, EDIT -> {}
                case EQUAL -> {
                    long value = var(c.input()).value;
                    var(c.output()).value = value;
                }
                case SCALE -> {
                    long scale = var(c.scale).value;
                    long offset = var(c.offset).value;
                    if (c.direction == FORWARD) {
                        var(c.second).value = var(c.first).value * scale + offset;
                    } else {
                        var(c.first).value = divEuclid(var(c.second).value - offset, scale);
                    }
                }
            }
        }

        long satisfy(long at, long mark) {
            chooseMethod(at, mark);
            if (!con(at).satisfied()) {
                return NONE;
            }
            markInputs(at, mark);
            long out = con(at).output();
            long overridden = var(out).determinedBy;
            if (overridden != NONE) {
                setDirection(overridden, UNDECIDED);
            }
            var(out).determinedBy = at;
            addPropagate(at);
            var(out).mark = mark;
            return overridden;
        }

        void incrementalAdd(long at) {
            long mark = newMark();
            long overridden = satisfy(at, mark);
            while (overridden != NONE) {
                overridden = satisfy(overridden, mark);
            }
        }

        void incrementalRemove(long at) {
            long out = con(at).output();
            setDirection(at, UNDECIDED);
            removeFromGraph(at);
            ArrayList<Long> unsatisfied = removePropagateFrom(out);
            for (long strength = REQUIRED; strength < WEAKEST; strength++) {
                for (long other : unsatisfied) {
                    if (con(other).strength == strength) {
                        incrementalAdd(other);
                    }
                }
            }
        }

        void destroy(long at) {
            if (con(at).satisfied()) {
                incrementalRemove(at);
            } else {
                removeFromGraph(at);
            }
        }

        void addPropagate(long at) {
            ArrayList<Long> todo = new ArrayList<>();
            todo.add(at);
            while (!todo.isEmpty()) {
                long next = todo.removeLast();
                recalculate(next);
                consuming(con(next).output(), todo);
            }
        }

        void consuming(long place, ArrayList<Long> into) {
            long determining = var(place).determinedBy;
            for (int at = 0; at < var(place).constraints.size(); at++) {
                long other = var(place).constraints.get(at);
                if (other != determining && con(other).satisfied()) {
                    into.add(other);
                }
            }
        }

        ArrayList<Long> removePropagateFrom(long place) {
            Variable variable = var(place);
            variable.determinedBy = NONE;
            variable.walk = WEAKEST;
            variable.stay = true;
            ArrayList<Long> unsatisfied = new ArrayList<>();
            ArrayList<Long> todo = new ArrayList<>();
            todo.add(place);
            while (!todo.isEmpty()) {
                long next = todo.removeLast();
                long determining = var(next).determinedBy;
                ArrayList<Long> constraints = new ArrayList<>(var(next).constraints);
                for (long other : constraints) {
                    if (!con(other).satisfied()) {
                        unsatisfied.add(other);
                    }
                }
                for (long other : constraints) {
                    if (other != determining && con(other).satisfied()) {
                        recalculate(other);
                        todo.add(con(other).output());
                    }
                }
            }
            return unsatisfied;
        }

        ArrayList<Long> planFrom(ArrayList<Long> sources) {
            long mark = newMark();
            ArrayList<Long> plan = new ArrayList<>();
            ArrayList<Long> todo = sources;
            while (!todo.isEmpty()) {
                long next = todo.removeLast();
                long out = con(next).output();
                if (var(out).mark != mark && inputsKnown(next, mark)) {
                    plan.add(next);
                    var(out).mark = mark;
                    consuming(out, todo);
                }
            }
            return plan;
        }

        void runPlan(ArrayList<Long> plan) {
            for (long at : plan) {
                execute(at);
            }
        }

        void change(long place, long value) {
            long edit = add(new Constraint(Kind.EDIT, PREFERRED, place, NONE));
            ArrayList<Long> sources = new ArrayList<>();
            sources.add(edit);
            ArrayList<Long> plan = planFrom(sources);
            for (int i = 0; i < 10; i++) {
                var(place).value = value;
                runPlan(plan);
            }
            destroy(edit);
        }
    }

    static long chainTest(int size) {
        Planner planner = new Planner();
        ArrayList<Long> places = new ArrayList<>();
        for (int i = 0; i <= size; i++) {
            places.add(planner.variable(0));
        }
        for (int i = 0; i < size; i++) {
            planner.add(new Constraint(Kind.EQUAL, REQUIRED, places.get(i), places.get(i + 1)));
        }
        long last = places.get(size);
        planner.add(new Constraint(Kind.STAY, STRONG_DEFAULT, last, NONE));
        long edit = planner.add(new Constraint(Kind.EDIT, PREFERRED, places.get(0), NONE));
        ArrayList<Long> sources = new ArrayList<>();
        sources.add(edit);
        ArrayList<Long> plan = planner.planFrom(sources);
        long total = 0;
        for (long value = 0; value < 100; value++) {
            planner.var(places.get(0)).value = value;
            planner.runPlan(plan);
            total += planner.var(last).value;
        }
        planner.destroy(edit);
        return total;
    }

    static long projectionTest(long size) {
        Planner planner = new Planner();
        long scale = planner.variable(10);
        long offset = planner.variable(1000);
        ArrayList<Long> sources = new ArrayList<>();
        ArrayList<Long> targets = new ArrayList<>();
        for (long i = 0; i < size; i++) {
            long source = planner.variable(i);
            long target = planner.variable(i);
            sources.add(source);
            targets.add(target);
            planner.add(new Constraint(Kind.STAY, NORMAL, source, NONE));
            Constraint scaled = new Constraint(Kind.SCALE, REQUIRED, source, target);
            scaled.scale = scale;
            scaled.offset = offset;
            planner.add(scaled);
        }
        planner.change(sources.get(0), 17);
        long total = planner.var(targets.get(0)).value;
        planner.change(targets.get(0), 1050);
        total += planner.var(sources.get(0)).value;
        planner.change(scale, 5);
        for (long target : targets) {
            total += planner.var(target).value;
        }
        planner.change(offset, 2000);
        for (long target : targets) {
            total += planner.var(target).value;
        }
        return total;
    }

    static void run(PrintStream out) {
        long size = 100;
        long total = 0;
        for (int i = 0; i < 100; i++) {
            total += chainTest((int) size) + projectionTest(size);
        }
        out.println(total);
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
