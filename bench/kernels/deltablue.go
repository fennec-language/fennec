// DeltaBlue, the Go twin of deltablue.fen.
package main

import (
	"fmt"
	"slices"
)

const NONE int64 = -1

const (
	REQUIRED       int64 = 0
	PREFERRED      int64 = 2
	STRONG_DEFAULT int64 = 3
	NORMAL         int64 = 4
	WEAKEST        int64 = 6
)

const (
	UNDECIDED int64 = 0
	FORWARD   int64 = 1
	BACKWARD  int64 = 2
)

type Kind int

const (
	Stay Kind = iota
	Edit
	Equal
	Scale
)

type Variable struct {
	value        int64
	constraints  []int64
	determinedBy int64
	mark         int64
	walk         int64
	stay         bool
}

type Constraint struct {
	kind      Kind
	strength  int64
	first     int64
	second    int64
	scale     int64
	offset    int64
	direction int64
}

func newConstraint(kind Kind, strength, first, second int64) Constraint {
	return Constraint{kind: kind, strength: strength, first: first, second: second, scale: NONE, offset: NONE, direction: UNDECIDED}
}

func (c Constraint) unary() bool {
	return c.kind == Stay || c.kind == Edit
}

func (c Constraint) satisfied() bool {
	return c.direction != UNDECIDED
}

func (c Constraint) output() int64 {
	if c.unary() || c.direction == BACKWARD {
		return c.first
	}
	return c.second
}

func (c Constraint) input() int64 {
	if c.direction == BACKWARD {
		return c.second
	}
	return c.first
}

type Planner struct {
	variables   []Variable
	constraints []Constraint
	currentMark int64
}

func (p *Planner) variable(value int64) int64 {
	p.variables = append(p.variables, Variable{
		value:        value,
		determinedBy: NONE,
		mark:         0,
		walk:         WEAKEST,
		stay:         true,
	})
	return int64(len(p.variables)) - 1
}

func (p *Planner) v(at int64) *Variable {
	return &p.variables[at]
}

func (p *Planner) con(at int64) Constraint {
	return p.constraints[at]
}

func (p *Planner) setDirection(at, direction int64) {
	p.constraints[at].direction = direction
}

func (p *Planner) newMark() int64 {
	p.currentMark++
	return p.currentMark
}

func (p *Planner) add(constraint Constraint) int64 {
	p.constraints = append(p.constraints, constraint)
	at := int64(len(p.constraints)) - 1
	p.addToGraph(at)
	p.incrementalAdd(at)
	return at
}

func (p *Planner) addToGraph(at int64) {
	c := p.con(at)
	first := p.v(c.first)
	first.constraints = append(first.constraints, at)
	if !c.unary() {
		second := p.v(c.second)
		second.constraints = append(second.constraints, at)
	}
	if c.scale != NONE {
		scale := p.v(c.scale)
		scale.constraints = append(scale.constraints, at)
		offset := p.v(c.offset)
		offset.constraints = append(offset.constraints, at)
	}
	p.setDirection(at, UNDECIDED)
}

func (p *Planner) removeFromGraph(at int64) {
	c := p.con(at)
	for _, place := range [4]int64{c.first, c.second, c.scale, c.offset} {
		if place != NONE {
			variable := p.v(place)
			variable.constraints = slices.DeleteFunc(variable.constraints, func(other int64) bool { return other == at })
			if variable.determinedBy == at {
				variable.determinedBy = NONE
			}
		}
	}
	p.setDirection(at, UNDECIDED)
}

func (p *Planner) chooseMethod(at, mark int64) {
	c := p.con(at)
	if c.unary() {
		first := p.v(c.first)
		taken := first.mark != mark && c.strength < first.walk
		if taken {
			p.setDirection(at, FORWARD)
		} else {
			p.setDirection(at, UNDECIDED)
		}
		return
	}
	firstMark, firstWalk := p.v(c.first).mark, p.v(c.first).walk
	secondMark, secondWalk := p.v(c.second).mark, p.v(c.second).walk
	direction := UNDECIDED
	if firstMark == mark {
		if secondMark != mark && c.strength < secondWalk {
			direction = FORWARD
		}
	} else if secondMark == mark {
		if firstMark != mark && c.strength < firstWalk {
			direction = BACKWARD
		}
	} else if firstWalk > secondWalk {
		if c.strength < firstWalk {
			direction = BACKWARD
		}
	} else if c.strength < secondWalk {
		direction = FORWARD
	}
	p.setDirection(at, direction)
}

func (p *Planner) markInputs(at, mark int64) {
	c := p.con(at)
	if c.unary() {
		return
	}
	p.v(c.input()).mark = mark
	if c.scale != NONE {
		p.v(c.scale).mark = mark
		p.v(c.offset).mark = mark
	}
}

func (p *Planner) inputsKnown(at, mark int64) bool {
	c := p.con(at)
	if c.unary() {
		return true
	}
	input := p.v(c.input())
	return input.mark == mark || input.stay || input.determinedBy == NONE
}

func (p *Planner) recalculate(at int64) {
	c := p.con(at)
	out := c.output()
	if c.unary() {
		output := p.v(out)
		output.walk = c.strength
		output.stay = c.kind != Edit
		return
	}
	input := c.input()
	walk := max(c.strength, p.v(input).walk)
	stay := p.v(input).stay
	if c.scale != NONE {
		stay = stay && p.v(c.scale).stay && p.v(c.offset).stay
	}
	output := p.v(out)
	output.walk = walk
	output.stay = stay
	if stay {
		p.execute(at)
	}
}

func divEuclid(a, b int64) int64 {
	q := a / b
	if a%b < 0 {
		if b > 0 {
			q--
		} else {
			q++
		}
	}
	return q
}

func (p *Planner) execute(at int64) {
	c := p.con(at)
	switch c.kind {
	case Stay, Edit:
	case Equal:
		value := p.v(c.input()).value
		p.v(c.output()).value = value
	case Scale:
		scale := p.v(c.scale).value
		offset := p.v(c.offset).value
		if c.direction == FORWARD {
			p.v(c.second).value = p.v(c.first).value*scale + offset
		} else {
			p.v(c.first).value = divEuclid(p.v(c.second).value-offset, scale)
		}
	}
}

func (p *Planner) satisfy(at, mark int64) int64 {
	p.chooseMethod(at, mark)
	if !p.con(at).satisfied() {
		return NONE
	}
	p.markInputs(at, mark)
	out := p.con(at).output()
	overridden := p.v(out).determinedBy
	if overridden != NONE {
		p.setDirection(overridden, UNDECIDED)
	}
	p.v(out).determinedBy = at
	p.addPropagate(at)
	p.v(out).mark = mark
	return overridden
}

func (p *Planner) incrementalAdd(at int64) {
	mark := p.newMark()
	overridden := p.satisfy(at, mark)
	for overridden != NONE {
		overridden = p.satisfy(overridden, mark)
	}
}

func (p *Planner) incrementalRemove(at int64) {
	out := p.con(at).output()
	p.setDirection(at, UNDECIDED)
	p.removeFromGraph(at)
	unsatisfied := p.removePropagateFrom(out)
	for strength := REQUIRED; strength < WEAKEST; strength++ {
		for _, other := range unsatisfied {
			if p.con(other).strength == strength {
				p.incrementalAdd(other)
			}
		}
	}
}

func (p *Planner) destroy(at int64) {
	if p.con(at).satisfied() {
		p.incrementalRemove(at)
	} else {
		p.removeFromGraph(at)
	}
}

func (p *Planner) addPropagate(at int64) {
	todo := []int64{at}
	for len(todo) > 0 {
		next := todo[len(todo)-1]
		todo = todo[:len(todo)-1]
		p.recalculate(next)
		todo = p.consuming(p.con(next).output(), todo)
	}
}

func (p *Planner) consuming(place int64, into []int64) []int64 {
	determining := p.v(place).determinedBy
	for at := 0; at < len(p.v(place).constraints); at++ {
		other := p.v(place).constraints[at]
		if other != determining && p.con(other).satisfied() {
			into = append(into, other)
		}
	}
	return into
}

func (p *Planner) removePropagateFrom(place int64) []int64 {
	variable := p.v(place)
	variable.determinedBy = NONE
	variable.walk = WEAKEST
	variable.stay = true
	var unsatisfied []int64
	todo := []int64{place}
	for len(todo) > 0 {
		next := todo[len(todo)-1]
		todo = todo[:len(todo)-1]
		determining := p.v(next).determinedBy
		constraints := slices.Clone(p.v(next).constraints)
		for _, other := range constraints {
			if !p.con(other).satisfied() {
				unsatisfied = append(unsatisfied, other)
			}
		}
		for _, other := range constraints {
			if other != determining && p.con(other).satisfied() {
				p.recalculate(other)
				todo = append(todo, p.con(other).output())
			}
		}
	}
	return unsatisfied
}

func (p *Planner) planFrom(sources []int64) []int64 {
	mark := p.newMark()
	var plan []int64
	todo := sources
	for len(todo) > 0 {
		next := todo[len(todo)-1]
		todo = todo[:len(todo)-1]
		out := p.con(next).output()
		if p.v(out).mark != mark && p.inputsKnown(next, mark) {
			plan = append(plan, next)
			p.v(out).mark = mark
			todo = p.consuming(out, todo)
		}
	}
	return plan
}

func (p *Planner) runPlan(plan []int64) {
	for _, at := range plan {
		p.execute(at)
	}
}

func (p *Planner) change(place, value int64) {
	edit := p.add(newConstraint(Edit, PREFERRED, place, NONE))
	plan := p.planFrom([]int64{edit})
	for range 10 {
		p.v(place).value = value
		p.runPlan(plan)
	}
	p.destroy(edit)
}

func chainTest(size int) int64 {
	planner := &Planner{}
	places := make([]int64, 0, size+1)
	for range size + 1 {
		places = append(places, planner.variable(0))
	}
	for i := 0; i < size; i++ {
		planner.add(newConstraint(Equal, REQUIRED, places[i], places[i+1]))
	}
	last := places[size]
	planner.add(newConstraint(Stay, STRONG_DEFAULT, last, NONE))
	edit := planner.add(newConstraint(Edit, PREFERRED, places[0], NONE))
	plan := planner.planFrom([]int64{edit})
	var total int64
	for value := int64(0); value < 100; value++ {
		planner.v(places[0]).value = value
		planner.runPlan(plan)
		total += planner.v(last).value
	}
	planner.destroy(edit)
	return total
}

func projectionTest(size int64) int64 {
	planner := &Planner{}
	scale := planner.variable(10)
	offset := planner.variable(1000)
	var sources, targets []int64
	for i := int64(0); i < size; i++ {
		source := planner.variable(i)
		target := planner.variable(i)
		sources = append(sources, source)
		targets = append(targets, target)
		planner.add(newConstraint(Stay, NORMAL, source, NONE))
		scaled := newConstraint(Scale, REQUIRED, source, target)
		scaled.scale = scale
		scaled.offset = offset
		planner.add(scaled)
	}
	planner.change(sources[0], 17)
	total := planner.v(targets[0]).value
	planner.change(targets[0], 1050)
	total += planner.v(sources[0]).value
	planner.change(scale, 5)
	for _, target := range targets {
		total += planner.v(target).value
	}
	planner.change(offset, 2000)
	for _, target := range targets {
		total += planner.v(target).value
	}
	return total
}

func main() {
	size := int64(100)
	var total int64
	for range 100 {
		total += chainTest(int(size)) + projectionTest(size)
	}
	fmt.Println(total)
}
