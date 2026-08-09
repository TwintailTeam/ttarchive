pub(crate) fn assign_lengths(freqs: &[u32], max_bits: usize) -> Vec<u8> {
    let n = freqs.len();
    let mut lengths = vec![0u8; n];

    let mut used: Vec<usize> = (0..n).filter(|&i| freqs[i] > 0).collect();

    match used.len() {
        0 => return lengths,
        1 => {
            let first = used[0];
            let filler = if first == 0 { 1 } else { 0 };
            lengths[first] = 1;
            if filler < n {
                lengths[filler] = 1;
            }
            return lengths;
        }
        _ => {}
    }

    used.sort_by(|&a, &b| freqs[a].cmp(&freqs[b]).then(a.cmp(&b)));

    for (symbol, length) in used.iter().zip(package_merge(&used, freqs, max_bits)) {
        lengths[*symbol] = length;
    }

    lengths
}

#[derive(Clone, Copy)]
struct Item {
    weight: u64,
    is_leaf: bool,
}

fn package_merge(used: &[usize], freqs: &[u32], max_bits: usize) -> Vec<u8> {
    let n = used.len();

    let leaves: Vec<Item> = used.iter().map(|&s| Item { weight: freqs[s] as u64, is_leaf: true }).collect();

    let mut levels: Vec<Vec<Item>> = Vec::with_capacity(max_bits);
    levels.push(leaves.clone());

    for l in 1..max_bits {
        let previous = &levels[l - 1];
        let mut packages = Vec::with_capacity(previous.len() / 2);
        for pair in previous.chunks_exact(2) {
            packages.push(Item { weight: pair[0].weight + pair[1].weight, is_leaf: false });
        }

        let mut merged = Vec::with_capacity(leaves.len() + packages.len());
        let (mut i, mut j) = (0, 0);
        while i < leaves.len() || j < packages.len() {
            let take_leaf = match (leaves.get(i), packages.get(j)) {
                (Some(leaf), Some(package)) => leaf.weight <= package.weight,
                (Some(_), None) => true,
                _ => false,
            };
            if take_leaf {
                merged.push(leaves[i]);
                i += 1;
            } else {
                merged.push(packages[j]);
                j += 1;
            }
        }
        levels.push(merged);
    }

    let mut lengths = vec![0u8; n];
    let mut take = 2 * n - 2;

    for level in levels.iter().rev() {
        let take_here = take.min(level.len());
        let leaves_taken = level[..take_here].iter().filter(|item| item.is_leaf).count();

        for length in lengths.iter_mut().take(leaves_taken) {
            *length += 1;
        }

        take = 2 * (take_here - leaves_taken);
    }

    lengths
}
