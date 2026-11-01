// Generators: stages chained through names and through functions, used by what asks for items.

struct Row {
    amount: i64,
    weight: i64,
}

fn parse(line: &str) -> Row {
    let length = line.chars().count() as i64;
    Row { amount: length, weight: length % 7 }
}

fn make_lines(count: i64) -> Vec<String> {
    (0..count)
        .map(|at| {
            if at % 5 == 0 {
                "   ".to_owned()
            } else if at % 4 == 0 {
                format!("  item {at} ")
            } else {
                format!("item {at}")
            }
        })
        .collect()
}

fn pipeline(src: &[String]) -> i64 {
    let lines = src.iter().map(|l| l.trim());
    let rows = lines.filter(|l| !l.is_empty()).map(parse);
    rows.map(|r| r.amount + r.weight).sum()
}

fn count_up(limit: i64) -> impl Iterator<Item = i64> {
    0..limit
}

fn odd_squares(numbers: impl Iterator<Item = i64>) -> impl Iterator<Item = i64> {
    numbers.filter(|n| n % 2 == 1).map(|n| n * n % 1000)
}

fn running(numbers: impl Iterator<Item = i64>, size: i64) -> impl Iterator<Item = i64> {
    let mut total = 0;
    numbers.enumerate().map(move |(at, n)| {
        total += n;
        if at as i64 % size == 0 {
            total %= 1000;
        }
        total
    })
}

fn first_over(limit: i64, over: i64) -> i64 {
    odd_squares(count_up(limit)).find(|n| *n > over).unwrap_or(0)
}

fn main() {
    let src = make_lines(20_000);
    let mut result = 0;
    for round in 0..40 {
        result += pipeline(&src);
        let chain = running(odd_squares(count_up(200_000 + round)), 16);
        result += chain.map(|n| n % 7).sum::<i64>();
        result += first_over(1_000, 990 - round);
    }
    println!("{result}");
}
