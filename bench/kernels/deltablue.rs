//! DeltaBlue, the twin of `deltablue.fen`.

const NONE: i64 = -1;

const REQUIRED: i64 = 0;
const PREFERRED: i64 = 2;
const STRONG_DEFAULT: i64 = 3;
const NORMAL: i64 = 4;
const WEAKEST: i64 = 6;

const UNDECIDED: i64 = 0;
const FORWARD: i64 = 1;
const BACKWARD: i64 = 2;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Stay,
    Edit,
    Equal,
    Scale,
}

struct Variable {
    value: i64,
    constraints: Vec<i64>,
    determined_by: i64,
    mark: i64,
    walk: i64,
    stay: bool,
}

#[derive(Clone, Copy)]
struct Constraint {
    kind: Kind,
    strength: i64,
    first: i64,
    second: i64,
    scale: i64,
    offset: i64,
    direction: i64,
}

impl Constraint {
    fn new(kind: Kind, strength: i64, first: i64, second: i64) -> Self {
        Constraint { kind, strength, first, second, scale: NONE, offset: NONE, direction: UNDECIDED }
    }

    fn unary(&self) -> bool {
        matches!(self.kind, Kind::Stay | Kind::Edit)
    }

    fn satisfied(&self) -> bool {
        self.direction != UNDECIDED
    }

    fn output(&self) -> i64 {
        if self.unary() || self.direction == BACKWARD { self.first } else { self.second }
    }

    fn input(&self) -> i64 {
        if self.direction == BACKWARD { self.second } else { self.first }
    }
}

struct Planner {
    variables: Vec<Variable>,
    constraints: Vec<Constraint>,
    current_mark: i64,
}

impl Planner {
    fn new() -> Self {
        Planner { variables: Vec::new(), constraints: Vec::new(), current_mark: 0 }
    }

    fn variable(&mut self, value: i64) -> i64 {
        self.variables.push(Variable {
            value,
            constraints: Vec::new(),
            determined_by: NONE,
            mark: 0,
            walk: WEAKEST,
            stay: true,
        });
        self.variables.len() as i64 - 1
    }

    fn var(&mut self, at: i64) -> &mut Variable {
        &mut self.variables[at as usize]
    }

    fn con(&self, at: i64) -> Constraint {
        self.constraints[at as usize]
    }

    fn set_direction(&mut self, at: i64, direction: i64) {
        self.constraints[at as usize].direction = direction;
    }

    fn new_mark(&mut self) -> i64 {
        self.current_mark += 1;
        self.current_mark
    }

    fn add(&mut self, constraint: Constraint) -> i64 {
        self.constraints.push(constraint);
        let at = self.constraints.len() as i64 - 1;
        self.add_to_graph(at);
        self.incremental_add(at);
        at
    }

    fn add_to_graph(&mut self, at: i64) {
        let c = self.con(at);
        self.var(c.first).constraints.push(at);
        if !c.unary() {
            self.var(c.second).constraints.push(at);
        }
        if c.scale != NONE {
            self.var(c.scale).constraints.push(at);
            self.var(c.offset).constraints.push(at);
        }
        self.set_direction(at, UNDECIDED);
    }

    fn remove_from_graph(&mut self, at: i64) {
        let c = self.con(at);
        for place in [c.first, c.second, c.scale, c.offset] {
            if place != NONE {
                let variable = self.var(place);
                variable.constraints.retain(|other| *other != at);
                if variable.determined_by == at {
                    variable.determined_by = NONE;
                }
            }
        }
        self.set_direction(at, UNDECIDED);
    }

    fn choose_method(&mut self, at: i64, mark: i64) {
        let c = self.con(at);
        if c.unary() {
            let first = self.var(c.first);
            let taken = first.mark != mark && c.strength < first.walk;
            self.set_direction(at, if taken { FORWARD } else { UNDECIDED });
            return;
        }
        let (first_mark, first_walk) = (self.var(c.first).mark, self.var(c.first).walk);
        let (second_mark, second_walk) = (self.var(c.second).mark, self.var(c.second).walk);
        let mut direction = UNDECIDED;
        if first_mark == mark {
            if second_mark != mark && c.strength < second_walk {
                direction = FORWARD;
            }
        } else if second_mark == mark {
            if first_mark != mark && c.strength < first_walk {
                direction = BACKWARD;
            }
        } else if first_walk > second_walk {
            if c.strength < first_walk {
                direction = BACKWARD;
            }
        } else if c.strength < second_walk {
            direction = FORWARD;
        }
        self.set_direction(at, direction);
    }

    fn mark_inputs(&mut self, at: i64, mark: i64) {
        let c = self.con(at);
        if c.unary() {
            return;
        }
        self.var(c.input()).mark = mark;
        if c.scale != NONE {
            self.var(c.scale).mark = mark;
            self.var(c.offset).mark = mark;
        }
    }

    fn inputs_known(&mut self, at: i64, mark: i64) -> bool {
        let c = self.con(at);
        if c.unary() {
            return true;
        }
        let input = self.var(c.input());
        input.mark == mark || input.stay || input.determined_by == NONE
    }

    fn recalculate(&mut self, at: i64) {
        let c = self.con(at);
        let out = c.output();
        if c.unary() {
            let output = self.var(out);
            output.walk = c.strength;
            output.stay = c.kind != Kind::Edit;
            return;
        }
        let input = c.input();
        let walk = c.strength.max(self.var(input).walk);
        let mut stay = self.var(input).stay;
        if c.scale != NONE {
            stay = stay && self.var(c.scale).stay && self.var(c.offset).stay;
        }
        let output = self.var(out);
        output.walk = walk;
        output.stay = stay;
        if stay {
            self.execute(at);
        }
    }

    fn execute(&mut self, at: i64) {
        let c = self.con(at);
        match c.kind {
            Kind::Stay | Kind::Edit => {}
            Kind::Equal => {
                let value = self.var(c.input()).value;
                self.var(c.output()).value = value;
            }
            Kind::Scale => {
                let scale = self.var(c.scale).value;
                let offset = self.var(c.offset).value;
                if c.direction == FORWARD {
                    self.var(c.second).value = self.var(c.first).value * scale + offset;
                } else {
                    self.var(c.first).value = (self.var(c.second).value - offset).div_euclid(scale);
                }
            }
        }
    }

    fn satisfy(&mut self, at: i64, mark: i64) -> i64 {
        self.choose_method(at, mark);
        if !self.con(at).satisfied() {
            return NONE;
        }
        self.mark_inputs(at, mark);
        let out = self.con(at).output();
        let overridden = self.var(out).determined_by;
        if overridden != NONE {
            self.set_direction(overridden, UNDECIDED);
        }
        self.var(out).determined_by = at;
        self.add_propagate(at);
        self.var(out).mark = mark;
        overridden
    }

    fn incremental_add(&mut self, at: i64) {
        let mark = self.new_mark();
        let mut overridden = self.satisfy(at, mark);
        while overridden != NONE {
            overridden = self.satisfy(overridden, mark);
        }
    }

    fn incremental_remove(&mut self, at: i64) {
        let out = self.con(at).output();
        self.set_direction(at, UNDECIDED);
        self.remove_from_graph(at);
        let unsatisfied = self.remove_propagate_from(out);
        for strength in REQUIRED..WEAKEST {
            for other in &unsatisfied {
                if self.con(*other).strength == strength {
                    self.incremental_add(*other);
                }
            }
        }
    }

    fn destroy(&mut self, at: i64) {
        if self.con(at).satisfied() {
            self.incremental_remove(at);
        } else {
            self.remove_from_graph(at);
        }
    }

    fn add_propagate(&mut self, at: i64) {
        let mut todo = vec![at];
        while let Some(next) = todo.pop() {
            self.recalculate(next);
            self.consuming(self.con(next).output(), &mut todo);
        }
    }

    fn consuming(&mut self, place: i64, into: &mut Vec<i64>) {
        let determining = self.var(place).determined_by;
        for at in 0..self.var(place).constraints.len() {
            let other = self.var(place).constraints[at];
            if other != determining && self.con(other).satisfied() {
                into.push(other);
            }
        }
    }

    fn remove_propagate_from(&mut self, place: i64) -> Vec<i64> {
        let variable = self.var(place);
        variable.determined_by = NONE;
        variable.walk = WEAKEST;
        variable.stay = true;
        let mut unsatisfied = Vec::new();
        let mut todo = vec![place];
        while let Some(next) = todo.pop() {
            let determining = self.var(next).determined_by;
            let constraints = self.var(next).constraints.clone();
            for other in &constraints {
                if !self.con(*other).satisfied() {
                    unsatisfied.push(*other);
                }
            }
            for other in &constraints {
                if *other != determining && self.con(*other).satisfied() {
                    self.recalculate(*other);
                    todo.push(self.con(*other).output());
                }
            }
        }
        unsatisfied
    }

    fn plan_from(&mut self, sources: Vec<i64>) -> Vec<i64> {
        let mark = self.new_mark();
        let mut plan = Vec::new();
        let mut todo = sources;
        while let Some(next) = todo.pop() {
            let out = self.con(next).output();
            if self.var(out).mark != mark && self.inputs_known(next, mark) {
                plan.push(next);
                self.var(out).mark = mark;
                self.consuming(out, &mut todo);
            }
        }
        plan
    }

    fn run_plan(&mut self, plan: &[i64]) {
        for at in plan {
            self.execute(*at);
        }
    }

    fn change(&mut self, place: i64, value: i64) {
        let edit = self.add(Constraint::new(Kind::Edit, PREFERRED, place, NONE));
        let plan = self.plan_from(vec![edit]);
        for _ in 0..10 {
            self.var(place).value = value;
            self.run_plan(&plan);
        }
        self.destroy(edit);
    }
}

fn chain_test(size: usize) -> i64 {
    let mut planner = Planner::new();
    let places: Vec<i64> = (0..=size).map(|_| planner.variable(0)).collect();
    for i in 0..size {
        planner.add(Constraint::new(Kind::Equal, REQUIRED, places[i], places[i + 1]));
    }
    let last = places[size];
    planner.add(Constraint::new(Kind::Stay, STRONG_DEFAULT, last, NONE));
    let edit = planner.add(Constraint::new(Kind::Edit, PREFERRED, places[0], NONE));
    let plan = planner.plan_from(vec![edit]);
    let mut total = 0;
    for value in 0..100 {
        planner.var(places[0]).value = value;
        planner.run_plan(&plan);
        total += planner.var(last).value;
    }
    planner.destroy(edit);
    total
}

fn projection_test(size: i64) -> i64 {
    let mut planner = Planner::new();
    let scale = planner.variable(10);
    let offset = planner.variable(1000);
    let (mut sources, mut targets) = (Vec::new(), Vec::new());
    for i in 0..size {
        let source = planner.variable(i);
        let target = planner.variable(i);
        sources.push(source);
        targets.push(target);
        planner.add(Constraint::new(Kind::Stay, NORMAL, source, NONE));
        let mut scaled = Constraint::new(Kind::Scale, REQUIRED, source, target);
        scaled.scale = scale;
        scaled.offset = offset;
        planner.add(scaled);
    }
    planner.change(sources[0], 17);
    let mut total = planner.var(targets[0]).value;
    planner.change(targets[0], 1050);
    total += planner.var(sources[0]).value;
    planner.change(scale, 5);
    for target in &targets {
        total += planner.var(*target).value;
    }
    planner.change(offset, 2000);
    for target in &targets {
        total += planner.var(*target).value;
    }
    total
}

fn main() {
    let size = std::hint::black_box(100);
    let mut total = 0;
    for _ in 0..5 {
        total += chain_test(size as usize) + projection_test(size);
    }
    println!("{total}");
}
