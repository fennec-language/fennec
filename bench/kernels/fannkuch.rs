//! Fannkuch, the twin of `fannkuch.fen`.

fn fannkuch(n: usize) -> (i64, i64) {
    let mut perm: Vec<usize> = (0..n).collect();
    let mut count = vec![0; n];
    let mut flips = vec![0; n];
    let (mut most, mut checksum, mut sign) = (0, 0, 1);
    loop {
        flips.copy_from_slice(&perm);
        let mut total = 0;
        let mut first = flips[0];
        while first != 0 {
            flips[..=first].reverse();
            total += 1;
            first = flips[0];
        }
        most = most.max(total);
        checksum += sign * total;
        sign = -sign;
        let mut r = 1;
        loop {
            if r == n {
                return (checksum, most);
            }
            perm[..=r].rotate_left(1);
            count[r] += 1;
            if count[r] <= r {
                break;
            }
            count[r] = 0;
            r += 1;
        }
    }
}

fn main() {
    let (checksum, most) = fannkuch(std::hint::black_box(9));
    println!("{checksum} {most}");
}
