use bio::alphabets;
use bio::data_structures::bwt::{bwt, less, Less, Occ, BWT};
use bio::data_structures::fmindex::{BackwardSearchResult, FMIndex, FMIndexable};
use bio::data_structures::suffix_array::{suffix_array, RawSuffixArray};
use bio::io::fasta;
use bio::io::fasta::Records;
use std::cmp::max;
use std::fs::File;
use std::io::BufReader;
use std::thread::current;

// const REFERENCE_PATH: &str = "data/sc2_reference.fasta";
// const QUERY_PATH: &str = "data/sc2_query_long.fasta";

const REFERENCE_PATH: &str = "/Users/corneliusromer/code/nextclade_data/data/datasets/rsv_a/references/EPI_ISL_412866/versions/2022-12-20T22:00:12Z/files/reference.fasta";
const QUERY_PATH: &str = "//Users/corneliusromer/code/nextclade_data/data/datasets/rsv_b/references/EPI_ISL_1653999/versions/2022-12-20T22:00:12Z/files/reference.fasta";

const SEED_MATCH_CONFIG: SeedMatchConfig = SeedMatchConfig {
    // Purposefully lax, to allow for some off-target matches
    kmer_length: 10,
    min_match_length: 20,
    allowed_mismatches: 4,
};

fn read_fasta(filename: &str) -> Records<BufReader<File>> {
    let path = std::path::Path::new(filename);
    let reader = fasta::Reader::new(File::open(path).unwrap());

    reader.records()
}

struct Index {
    fm_index: FMIndex<BWT, Less, Occ>,
    ref_seq: Vec<u8>,
    suffix_array: RawSuffixArray,
}

struct SeedMatchConfig {
    kmer_length: usize,
    min_match_length: usize,
    allowed_mismatches: usize,
}

#[derive(Clone, Copy, Debug)]
struct SeedMatch {
    ref_index: usize,
    qry_index: usize,
    length: usize,
    mismatches: usize,
}

impl SeedMatch {
    fn offset(&self) -> isize {
        self.ref_index as isize - self.qry_index as isize
    }

    fn shift(&self, other: &SeedMatch) -> isize {
        self.offset() - other.offset()
    }

    fn qry_shift(&self, other: &SeedMatch) -> isize {
        self.qry_index as isize - other.qry_index as isize
    }

    fn qry_end(&self) -> usize {
        self.qry_index + self.length
    }

    fn ref_end(&self) -> usize {
        self.ref_index + self.length
    }
}

impl Index {
    /// Creates a new FMindex from a reference sequence
    fn from_sequence(mut reference: Vec<u8>) -> Self {
        let alphabet = alphabets::dna::iupac_alphabet();

        //Need to end the sequence that's indexed with the special/magic character `$`
        //otherwise the index doesn't work
        reference.push(b'$');

        let suffix_array = suffix_array(&reference);
        let burrow_wheeler_transform = bwt(&reference, &suffix_array);
        let less = less(&burrow_wheeler_transform, &alphabet);
        let occ = Occ::new(&burrow_wheeler_transform, 1, &alphabet);
        let fm_index = FMIndex::new(burrow_wheeler_transform, less, occ);

        reference.pop();

        Self {
            fm_index,
            ref_seq: reference,
            suffix_array,
        }
    }

    /// Internal function to perform a backward search on the index
    fn backward_search(&self, query: &[u8]) -> BackwardSearchResult {
        self.fm_index.backward_search(query.iter())
    }

    /// Returns the starting indeces of all full matches of the query in the reference
    /// Returns an empty vector if no matches are found
    fn full_matches(&self, query: &[u8]) -> Vec<usize> {
        let backward_search_result = self.backward_search(query);
        match backward_search_result {
            BackwardSearchResult::Complete(suffix_array_interval) => {
                suffix_array_interval.occ(&self.suffix_array)
            }
            _ => Vec::<usize>::new(),
        }
    }

    /// Returns matching index if there is exactly one match, otherwise returns None
    fn single_match(&self, kmer: &[u8]) -> Option<usize> {
        let matches = self.full_matches(kmer);
        if matches.len() == 1 {
            Some(matches[0])
        } else {
            None
        }
    }

    /// Given a seed, extends forwards and backwards and returns the extended seed
    fn extend_seed(
        &self,
        qry_seq: &[u8],
        input_match: &SeedMatch,
        config: &SeedMatchConfig,
    ) -> SeedMatch {
        let SeedMatch {
            mut ref_index,
            mut qry_index,
            mut length,
            mismatches,
        } = input_match.clone();

        let mut forward_mismatches = 0;

        while forward_mismatches < config.allowed_mismatches
            && ref_index + length < self.ref_seq.len()
            && qry_index + length < qry_seq.len()
        {
            if self.ref_seq[ref_index + length] != qry_seq[qry_index + length] {
                forward_mismatches += 1;
            }

            length += 1;
        }

        let mut backward_mismatches = 0;

        while backward_mismatches < config.allowed_mismatches && ref_index > 0 && qry_index > 0 {
            if self.ref_seq[ref_index - 1] != qry_seq[qry_index - 1] {
                backward_mismatches += 1;
            }

            ref_index -= 1;
            qry_index -= 1;
            length += 1;
        }

        SeedMatch {
            qry_index,
            ref_index,
            length,
            mismatches: mismatches + forward_mismatches + backward_mismatches,
        }
    }

    fn single_extended_match(
        &self,
        qry_seq: &[u8],
        qry_index: usize,
        config: &SeedMatchConfig,
    ) -> Option<SeedMatch> {
        let Some(kmer) = &qry_seq.get(qry_index..qry_index + config.kmer_length) else {
            return None;
        };

        if let Some(ref_index) = self.single_match(&kmer) {
            Some(self.extend_seed(
                qry_seq,
                &SeedMatch {
                    qry_index,
                    ref_index,
                    length: config.kmer_length,
                    mismatches: 0,
                },
                config,
            ))
        } else {
            None
        }
    }
}

struct CodonSpacedIndex {
    indexes: [Index; 3],
    ref_seq: Vec<u8>,
}

// every third position of reference

impl CodonSpacedIndex {
    fn from_sequence(reference: Vec<u8>) -> Self {
        let indexes = [
            Index::from_sequence(reference.iter().step_by(3).copied().collect()),
            Index::from_sequence(reference.iter().skip(1).step_by(3).copied().collect()),
            Index::from_sequence(reference.iter().skip(2).step_by(3).copied().collect()),
        ];

        Self {
            indexes,
            ref_seq: reference,
        }
    }

    fn extended_matches(&self, qry_seq: &[u8], config: &SeedMatchConfig) -> Vec<SeedMatch> {
        let mut matches = Vec::<SeedMatch>::new();

        let spaced_queries: [Vec<u8>; 3] = [
            qry_seq.iter().step_by(3).copied().collect(),
            qry_seq.iter().skip(1).step_by(3).copied().collect(),
            qry_seq.iter().skip(2).step_by(3).copied().collect(),
        ];

        for (i, qry_seq) in spaced_queries.iter().enumerate() {
            for (j, index) in self.indexes.iter().enumerate() {
                let mut qry_index = 0;
                while qry_index < qry_seq.len() {
                    if let Some(seed_match) =
                        index.single_extended_match(qry_seq, qry_index, config)
                    {
                        if seed_match.length > config.min_match_length {
                            matches.push(SeedMatch {
                                qry_index: seed_match.qry_index * 3 + i,
                                ref_index: seed_match.ref_index * 3 + j,
                                length: 3 * seed_match.length,
                                mismatches: seed_match.mismatches,
                            });
                            qry_index += 1;
                        }
                    }
                    qry_index += 1;
                }
            }
        }
        matches
    }
}

/// Combine overlapping seed matches that have the same offset
fn combine_seeds(mut matches: Vec<SeedMatch>) -> Vec<SeedMatch> {
    matches.sort_by(|a, b| a.qry_index.cmp(&b.qry_index));

    let mut combined_matches = Vec::<SeedMatch>::with_capacity(matches.len());

    for i in 0..matches.len() {
        let mut current_match = matches[i].clone();
        let next_match = matches.get(i + 1);
        for next_match in matches.iter().skip(i + 1) {
            if next_match.qry_index > current_match.qry_index + current_match.length {
                break;
            }
            if next_match.shift(&current_match) != 0 {
                continue;
            }
            current_match.length = max(
                current_match.length,
                next_match.qry_shift(&current_match) as usize + next_match.length,
            );
        }
        if current_match.qry_index
            > combined_matches
                .last()
                .map(|m| m.qry_index + m.length)
                .unwrap_or(0)
        {
            combined_matches.push(current_match);
        }
    }
    combined_matches
}

/// Chain seeds using algorithm in "Algorithms on Strings, Trees and Sequences" by Dan Gusfield, chapter 13.3, page 326, "The two-dimensional chain problem"
fn chain_seeds(matches: Vec<SeedMatch>) {
    #[derive(Clone, Copy, Debug)]
    struct Triplet {
        ref_end: usize,
        score: usize,
        j: usize,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum EndpointSide {
        Start,
        End,
    }

    #[derive(Clone, Copy, Debug)]
    struct Endpoint {
        qry_pos: usize,
        side: EndpointSide,
        j: usize,
    }

    let mut scores: Vec<usize> = vec![0; matches.len()];

    // Construct endpoint vec
    let mut endpoints = Vec::<Endpoint>::with_capacity(2 * matches.len());
    for (match_no, match_) in matches.iter().enumerate() {
        endpoints.push(Endpoint {
            qry_pos: match_.qry_index.to_owned(),
            side: EndpointSide::Start,
            j: match_no,
        });
        endpoints.push(Endpoint {
            qry_pos: match_.qry_index + match_.length,
            side: EndpointSide::End,
            j: match_no,
        });
    }
    // dbg!(&endpoints);

    endpoints.sort_by(|a, b| a.qry_pos.cmp(&b.qry_pos));

    let mut triplets = Vec::<Triplet>::with_capacity(matches.len());

    for endpoint in endpoints {
        // dbg!(endpoint);
        // if endpoint.side == EndpointSide::End {
        //     dbg!(scores[endpoint.j]);
        // }
        // dbg!(&matches[endpoint.j]);
        // dbg!(&triplets);
        match endpoint.side {
            EndpointSide::Start => {
                // Find first triplet where ref_end is > endpoint

                let mut first_triplet: &Triplet = &Triplet {
                    ref_end: 0,
                    score: 0,
                    j: 0,
                };
                for triplet in triplets.as_slice() {
                    if triplet.ref_end < matches[endpoint.j].ref_index {
                        first_triplet = triplet;
                        break;
                    }
                }
                scores[endpoint.j] = matches[endpoint.j].length + first_triplet.score;
            }
            EndpointSide::End => {
                let mut first_triplet: &Triplet = &Triplet {
                    ref_end: 0,
                    score: 0,
                    j: 0,
                };
                for triplet in triplets.as_slice() {
                    first_triplet = triplet;
                    if triplet.ref_end <= matches[endpoint.j].ref_index + matches[endpoint.j].length
                    {
                        break;
                    }
                }
                if scores[endpoint.j] > first_triplet.score {
                    let added_triplet = Triplet {
                        ref_end: matches[endpoint.j].ref_index + matches[endpoint.j].length,
                        score: scores[endpoint.j],
                        j: endpoint.j,
                    };
                    triplets.push(added_triplet.clone());
                    // Sort descending
                    triplets.sort_by(|b, a| a.ref_end.cmp(&b.ref_end));
                    triplets.retain(|triplet| {
                        triplet.ref_end < added_triplet.ref_end
                            || triplet.score >= added_triplet.score
                    });
                }
            }
        }
    }

    dbg!(triplets);
}

fn main() {
    // Should be abstracted away for our purposes into a class/structure
    // Allow initialization with one of:
    // - Path of fastas [take first] (for convenience)
    // - vec of u8 chars (interfacing with Nextclade)
    // Save everything internally
    // fn fm_index.full_matches(subsequence) -> vec[int] (indeces of full matches)

    // for logging: fn fm_index.best_matches(subsequence) -> IndexSearchResult::{Complete(vec[starting_indeces],Partial(vec[start,len)),Absent}

    // Optional: Abstraction of groups of 3s, same interface, return index of matches spaced by 3 (0::3::-1,1::3::-1,2::3::-1)
    // Optional: extension around the edges of matches, allow partial mismatches and extend from there - start with accepting only full matches
    // Get chains of matches, need to optimize some loss function through dynamical programming
    // Especially tricky: duplications (like in RSV)

    let reference = read_fasta(REFERENCE_PATH).next().unwrap().unwrap();
    let query = read_fasta(QUERY_PATH);

    let index = CodonSpacedIndex::from_sequence(reference.seq().to_vec());

    for record in query {
        let record = record.expect("Problem parsing sequence");
        println!("Processing sequence: {}", record.id());

        let mut matches = index.extended_matches(record.seq(), &SEED_MATCH_CONFIG);

        matches = combine_seeds(matches);

        for seed_match in matches.iter() {
            println!(
                "diff:{:>5} len:{:>7} ref_index:{:>7} qry_index:{:>7}",
                seed_match.ref_index as isize - seed_match.qry_index as isize,
                seed_match.length,
                seed_match.ref_index,
                seed_match.qry_index,
                // qry_left,
                // qry_right,
                // ref_left,
                // ref_right,
                // reference.seq()[ref_left..ref_right]
                //     .iter()
                //     .map(|&x| x as char)
                //     .collect::<String>()
            );
        }

        println!(
            "Covered: {}",
            matches.iter().fold(0, |acc, m| acc + m.length)
        );

        chain_seeds(matches);
        // Post process matches: join overlapping, remove short ones, remove outliers
        // Design windows based on joined matches, allowing bands, somewhat into the matches where there are gaps
    }
}
