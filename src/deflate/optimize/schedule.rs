// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Andrew C.E. Dent <https://github.com/ace-dent>

//! Route scheduling for one raw Deflate stream.
//!
//! [`optimize_raw_prefix_with_floor_and_grace`] validates the source, then
//! [`select_candidate`] runs the route phases in a fixed order. Every phase
//! reads the immutable [`RouteContext`] and updates the complete lineages in
//! [`RouteState`]. The order is part of the output contract: equal candidates
//! keep the earlier incumbent, and each optional route checks its deadline at
//! the point where it would start.
//!
//! Route algorithms, eligibility rules, and candidate builders remain in the
//! parent module. This module decides only which of them run, when, on which
//! worker, and which complete result each lineage retains.

use std::thread;
use std::time::{Duration, Instant};

use crate::progress::{
    reports_enabled, BalancedTreeProgress, CandidateProgress, Progress, SameDistanceProgress,
    StreamProgress,
};
use crate::{Error, Options, Result};

use super::{
    bounded_parallel_source_max_work_class, bounded_png_max_policy, build_apng_default_candidate,
    build_bounded_floor_candidate, build_bounded_generic_max_candidates,
    build_bounded_phase_candidates, build_candidate, build_candidate_from_plans,
    build_compact_proven_feedback_candidate, build_complementary_narrow_source_candidate,
    build_complete_apng_default_floor_candidate, build_complete_default_floor_candidate,
    build_deft4j_source_candidate, build_prepared_compact_source_split_floors,
    build_source_max_candidate, candidate_exposes_new_parent, candidate_progress,
    capture_source_block_report, changed_narrow_parent_should_continue,
    compact_balanced_tree_source_eligible, compact_dependent_deft4j_work_class,
    compact_split_parent_is_completed, complete_png_parallel_source_max_work_class,
    deft4j_source_route_eligible, established_floor_candidate, floor_exposes_new_search_states,
    floor_seeded_priority_with_structural_sibling, gain_is_below, has_multiple_nonempty_blocks,
    improve_default_floor_with_feedback, improve_with_terminal_searches,
    improve_with_terminal_tree_floors, independent_deft4j_refinement_can_overlap,
    is_strictly_better, join_route, narrow_source_route_eligible, normalize_258_aliases,
    parallel_route_is_bounded, parse_validated_rewrite, prebuild_bounded_floor,
    prepare_compact_source_split_seed, refine_bounded_deft4j_lineage,
    refine_with_compact_balanced_tree_floor, refine_with_compact_proven_feedback,
    refine_with_compact_source_split_floor, refine_with_compact_source_split_floor_until,
    refine_with_max_planner, refine_with_no_split_route,
    refine_with_terminal_source_split_floor_until, replace_optional_if_smaller,
    source_run_match_count_exceeds, spawn_route, BoundedFollowUpCandidates, BoundedPhaseCandidates,
    BoundedPngMaxPolicy, BoundedRoutes, Candidate, CandidateInput, CompactSplitSeed, DefaultFloor,
    DefaultFloorWork, RawInfo, RawOptimization, ReplayPlanner, StreamIdentity,
    COMPACT_SPLIT_FLOOR_MAX_COMPRESSED, DEFAULT_RAW_REPLAY_LIMIT, MAX_RAW_REPLAY_LIMIT,
    WEAK_DEFT4J_GAIN_BASIS_POINTS,
};
use crate::deflate::header::{balanced_tree_opportunities, BalancedTreeOpportunities};
use crate::deflate::model::{ParsedBlock, ParsedStream};
use crate::deflate::parse::parse_stream;
use crate::deflate::search::{
    compact_proven_submatch_route_eligible, same_distance_opportunities,
    PROVEN_SUBMATCH_FULL_MATCH_LIMIT,
};
use crate::deflate::stop::{initial_bounded_phase_share, Deadline, SearchStop};
use crate::deflate::stream::fragmented_collect_seed;

pub(crate) fn optimize_raw_prefix_with_floor_and_grace(
    input: &[u8],
    options: &Options,
    decoded_limit: u64,
    default_floor: DefaultFloor,
    grace: Duration,
) -> Result<RawOptimization> {
    if input.len() as u64 > options.max_input_bytes {
        return Err(Error::resource_limit(
            "input exceeds configured safety limit",
        ));
    }

    let started = Instant::now();
    let mut parsed = parse_stream(input, decoded_limit.min(options.max_decoded_bytes))?;
    // Avoid even an extra clock read in ordinary speed-first runs.
    let reporting = reports_enabled(options);
    let parse_elapsed = if reporting {
        started.elapsed()
    } else {
        std::time::Duration::ZERO
    };
    let mut blocks = std::mem::take(&mut parsed.blocks);
    let source_report = capture_source_block_report(&blocks, parsed.source_block_count, reporting);
    let progress = Progress::begin(
        options,
        started,
        StreamProgress {
            blocks: parsed.source_block_count,
            compressed_bytes: parsed.consumed,
            decoded_bytes: parsed.decoded_size,
            empty_blocks: parsed.source_empty_block_count,
            meaningful_bits: parsed.meaningful_bits,
            parse_elapsed,
        },
        source_report,
    );
    prepare_source_blocks(options, progress, &mut blocks)?;
    progress.routes();
    let deadlines = StreamDeadlines::new(started, grace, options, default_floor, &parsed);

    // Prefix callers need the exact bytes occupied by the first stream. Any
    // unused high bits in its final byte belong to that stream's byte-level
    // representation and are retained when the source wins.
    let original = &input[..parsed.consumed];
    let decoded_limit = decoded_limit.min(options.max_decoded_bytes);
    let identity = StreamIdentity {
        decoded_size: parsed.decoded_size,
        crc32: parsed.crc32,
        adler32: parsed.adler32,
    };
    let source = CandidateInput {
        compressed: original,
        blocks: &blocks,
        meaningful_bits: parsed.meaningful_bits,
        decoded_limit,
        identity,
    };
    let context = RouteContext {
        source,
        options,
        default_floor,
        deadline: &deadlines.primary,
        progress,
    };
    let mut candidate = select_candidate(&context, &deadlines)?;

    let keep_original = !options.strict && !candidate.is_strictly_smaller_than_source(source);
    let deflate_bits = if keep_original {
        parsed.meaningful_bits
    } else {
        candidate.bits
    };
    let final_report = if keep_original {
        capture_source_block_report(&blocks, parsed.source_block_count, reporting)
    } else {
        candidate.block_report.take()
    };
    let selected_route = if keep_original {
        "Original source"
    } else {
        candidate.route
    };
    let output_bytes = if keep_original {
        original.len()
    } else {
        candidate.data.len()
    };
    let output_max_distance = if keep_original {
        parsed.max_distance
    } else if let Some(max_distance) = candidate.output_max_distance {
        max_distance
    } else {
        // A known-losing child normally avoids a redundant validation parse,
        // but an outer route can still select its bytes over an older source.
        // Validate that uncommon final selection now and retain the metric
        // from the exact bytes that the container will wrap.
        parse_validated_rewrite(&candidate.data, decoded_limit, identity)?.max_distance
    };
    debug_assert!(output_max_distance <= parsed.max_distance);
    let timed_out = default_floor != DefaultFloor::MandatoryComplete
        && deadlines.terminal_work().was_triggered();
    progress.blocks(final_report);
    progress.finish(
        selected_route,
        output_bytes,
        deflate_bits,
        parsed.meaningful_bits,
        timed_out,
    );

    // Planning is complete. Drop model storage before copying a winning source
    // stream, then reuse the generated output allocation where possible. This
    // keeps the no-growth guarantee from briefly requiring two source-sized
    // outputs plus the full parsed model.
    drop(blocks);
    drop(std::mem::take(&mut candidate.plans));
    if keep_original {
        candidate.data.clear();
        candidate
            .data
            .try_reserve_exact(original.len())
            .map_err(|_| Error::internal("could not allocate Deflate output"))?;
        candidate.data.extend_from_slice(original);
    }
    let mut data = candidate.data;
    if options.strip_metadata {
        crate::deflate::parse::normalize_padding(&mut data, decoded_limit)?;
    }

    Ok(RawOptimization {
        data,
        consumed: parsed.consumed,
        info: RawInfo {
            crc32: parsed.crc32,
            adler32: parsed.adler32,
            size: parsed.decoded_size,
            max_distance: parsed.max_distance,
            source_deflate_bits: parsed.meaningful_bits,
            deflate_bits,
            source_block_count: parsed.source_block_count,
            source_empty_block_count: parsed.source_empty_block_count,
        },
        output_max_distance,
        timed_out,
    })
}

/// Immutable inputs shared by every scheduling phase for one raw stream.
#[derive(Clone, Copy)]
struct RouteContext<'a> {
    source: CandidateInput<'a>,
    options: &'a Options,
    default_floor: DefaultFloor,
    /// Primary optional-route deadline; see [`StreamDeadlines`].
    deadline: &'a Deadline,
    progress: Progress,
}

impl<'a> RouteContext<'a> {
    /// Report a complete candidate against the parsed source stream.
    fn report(&self, candidate: &Candidate) -> CandidateProgress {
        candidate_progress(
            candidate,
            self.source.meaningful_bits,
            candidate.is_strictly_smaller_than_source(self.source),
        )
    }

    fn report_optional(&self, candidate: Option<&Candidate>) -> Option<CandidateProgress> {
        candidate.map(|candidate| self.report(candidate))
    }

    /// Ordinary floor work governed by the primary deadline.
    fn timed(&self) -> DefaultFloorWork<'a> {
        DefaultFloorWork::Timed(self.deadline)
    }
}

/// The primary route deadline and the complete file allowance.
///
/// When a stream reserves terminal search, optional routes use a shorter
/// primary window without grace, and the full allowance governs only
/// terminal work and final timeout reporting. Otherwise both deadlines cover
/// the same allowance and the primary deadline governs everything.
struct StreamDeadlines {
    primary: Deadline,
    terminal: Deadline,
    reserve_terminal: bool,
}

impl StreamDeadlines {
    fn new(
        started: Instant,
        grace: Duration,
        options: &Options,
        default_floor: DefaultFloor,
        parsed: &ParsedStream,
    ) -> Self {
        let terminal = Deadline::with_grace(started, options.timeout, grace);
        let reserve_terminal = default_floor.reserves_terminal_search(
            options,
            parsed.consumed,
            parsed.decoded_size,
            parsed.source_block_count,
        );
        // A multi-image APNG child keeps nineteen twentieths of its assigned
        // slice for primary work; its smaller terminal share grows with time
        // and cannot consume another frame's slice. A stream owning the file
        // clock keeps four fifths for primary work. No phase grace may consume
        // either terminal share; finalization alone retains the original grace.
        let primary = if reserve_terminal {
            let primary_share = if default_floor == DefaultFloor::ApngMax {
                options.timeout.saturating_mul(19) / 20
            } else {
                initial_bounded_phase_share(options.timeout)
            };
            Deadline::with_grace(started, primary_share, Duration::ZERO)
        } else {
            Deadline::with_grace(started, options.timeout, grace)
        };
        Self {
            primary,
            terminal,
            reserve_terminal,
        }
    }

    /// The deadline governing terminal work and final timeout reporting.
    ///
    /// A primary phase yield forwards the incumbent without marking the file
    /// timed out. Only the full allowance governs terminal work and reporting.
    fn terminal_work(&self) -> &Deadline {
        if self.reserve_terminal {
            &self.terminal
        } else {
            &self.primary
        }
    }
}

/// Route families admitted by the parsed source and the floor policy.
///
/// These are pure topology and size checks. Deadline checks stay at each
/// route's start, so elapsed time never changes which families are eligible.
struct RouteEligibility {
    compact_tree: bool,
    compact_proven_feedback: bool,
    deft4j: bool,
    narrow_source: bool,
    parallel_routes: bool,
}

impl RouteEligibility {
    fn new(context: &RouteContext<'_>) -> Self {
        let RouteContext {
            source,
            options,
            default_floor,
            ..
        } = *context;
        let bounded_png_max = options.exhaustive && default_floor.uses_bounded_png_routes();
        Self {
            compact_tree: bounded_png_max
                && compact_balanced_tree_source_eligible(
                    source.compressed.len(),
                    source.identity.decoded_size,
                    source.blocks,
                ),
            compact_proven_feedback: bounded_png_max
                && source.blocks.len() == 1
                && compact_proven_submatch_route_eligible(
                    &source.blocks[0].tokens,
                    source.blocks[0].plain.len(),
                ),
            // The direct source graph is a Max quality route, not a PNG-only
            // specialization. Container scheduling may decide when it runs,
            // but no accepted Huffman/stored topology is permanently excluded:
            // otherwise a longer timeout could never recover a compatible
            // deft4j endpoint.
            deft4j: options.exhaustive && deft4j_source_route_eligible(source.blocks),
            // Bounded PNG routes share the parsed stream and one deadline.
            // Streams without a specialized source sibling run source max
            // beside the floor lineage; multi-block floors may also receive one
            // deterministic Columbo grouping pass. Standalone streams keep the
            // same route order without overlapping their larger working sets.
            narrow_source: bounded_png_max
                && narrow_source_route_eligible(source.blocks, source.compressed.len()),
            parallel_routes: options.exhaustive
                && default_floor.is_bounded()
                && parallel_route_is_bounded(source),
        }
    }
}

/// Complete candidates and scheduling facts carried between route phases.
///
/// Each candidate field is an independently retained lineage. Phases replace
/// a lineage only with a strictly smaller complete candidate unless a comment
/// at the assignment says otherwise.
#[derive(Default)]
struct RouteState {
    /// The exact ordinary endpoint that Max promises never to lose.
    complete_default: Option<Candidate>,
    /// The ordinary comparison floor, which also seeds Max's historical routes.
    floor: Option<Candidate>,
    floor_seeded: Option<Candidate>,
    deft4j: Option<Candidate>,
    narrow: Option<Candidate>,
    source_max: Option<Candidate>,
    /// Consumed by [`run_proven_feedback_floor`], which merges it into `floor`.
    proven_feedback: Option<Candidate>,
    compact_split: Option<Candidate>,
    split_seeds: CompactSplitSeeds,
    completed_compact_split_parent: Option<Vec<u8>>,
    /// The direct deft4j route saved less than the material-gain threshold on
    /// a multi-block bounded PNG. This admits both the deft4j floor seed and
    /// compact split inspection.
    weak_deft4j: bool,
    integrated_compact_source_max: bool,
    suppress_later_source_max: bool,
    suppress_later_optional_routes: bool,
    compact_split_attempted: bool,
    deft4j_refinement_completed: bool,
}

/// Prepared compact-split parents, one per distinct structural lineage.
#[derive(Default)]
struct CompactSplitSeeds {
    normal: Option<CompactSplitSeed>,
    seeded: Option<CompactSplitSeed>,
    deft4j: Option<CompactSplitSeed>,
}

impl CompactSplitSeeds {
    fn as_refs(&self) -> [Option<&CompactSplitSeed>; 3] {
        [
            self.normal.as_ref(),
            self.seeded.as_ref(),
            self.deft4j.as_ref(),
        ]
    }

    fn any(&self) -> bool {
        self.as_refs().into_iter().any(|seed| seed.is_some())
    }

    fn clear(&mut self) {
        *self = Self::default();
    }

    /// Price every prepared parent's compact split descendants.
    fn build(&self, context: &RouteContext<'_>) -> Result<Option<Candidate>> {
        build_prepared_compact_source_split_floors(
            self.as_refs(),
            context.options,
            context.source.decoded_limit,
            context.source.identity,
            context.deadline,
        )
    }
}

fn nonempty_block_count(blocks: &[ParsedBlock]) -> usize {
    blocks
        .iter()
        .filter(|block| !block.plain.is_empty())
        .count()
}

/// Normalize strict-mode aliases and report source opportunities.
fn prepare_source_blocks(
    options: &Options,
    progress: Progress,
    blocks: &mut [ParsedBlock],
) -> Result<()> {
    if options.strict {
        let normalization_started = progress.enabled().then(Instant::now);
        let normalized_blocks = normalize_258_aliases(blocks)?;
        if let Some(normalization_started) = normalization_started {
            progress.normalization(normalized_blocks, normalization_started.elapsed());
        }
    }
    if progress.enabled() {
        let opportunities = same_distance_opportunities(blocks);
        progress.same_distance_opportunities(SameDistanceProgress {
            runs: opportunities.runs,
            matches: opportunities.matches,
            decoded_bytes: opportunities.decoded_bytes,
            coalescible_runs: opportunities.coalescible_runs,
            repartition_runs: opportunities.repartition_runs,
            tokens_removable: opportunities.tokens_removable,
        });
        let mut tree_opportunities = BalancedTreeOpportunities::default();
        for block in blocks.iter() {
            let Some(seed) = block.original_dynamic.as_ref() else {
                continue;
            };
            if let Some(opportunities) = balanced_tree_opportunities(
                &block.literal_frequencies,
                &block.distance_frequencies,
                seed,
            ) {
                tree_opportunities.add_assign(opportunities);
            }
        }
        progress.balanced_tree_opportunities(BalancedTreeProgress {
            dynamic_blocks: tree_opportunities.dynamic_blocks,
            literal_pair_moves: tree_opportunities.literal_pair_moves,
            literal_quad_moves: tree_opportunities.literal_quad_moves,
            distance_pair_moves: tree_opportunities.distance_pair_moves,
            distance_quad_moves: tree_opportunities.distance_quad_moves,
            paired_prices: tree_opportunities.paired_prices,
        });
    }
    Ok(())
}

/// Run every admitted route family and return the selected complete candidate.
///
/// The bounded phases (through [`close_no_split_lineage`]) build independent
/// Max lineages beside the ordinary floor. [`establish_comparison_floor`] then
/// fixes the incumbent, and each later phase may only replace it with a
/// strictly smaller complete candidate before the terminal searches run.
fn select_candidate(context: &RouteContext<'_>, deadlines: &StreamDeadlines) -> Result<Candidate> {
    let mut state = RouteState::default();
    let guaranteed_floor = build_guaranteed_floor(context, &mut state)?;
    let eligibility = RouteEligibility::new(context);
    run_bounded_phase(context, &eligibility, guaranteed_floor, &mut state)?;
    run_proven_feedback_floor(context, &eligibility, &mut state)?;
    prepare_compact_split_seeds(context, &mut state)?;
    finish_floor_seeded_tree(context, &eligibility, &mut state)?;
    continue_floor_seeded(context, &eligibility, &mut state)?;
    run_bounded_refinement(context, &eligibility, &mut state)?;
    close_no_split_lineage(context, &mut state)?;

    let mut candidate = establish_comparison_floor(context, &mut state)?;
    merge_independent_topologies(context, &eligibility, &mut state, &mut candidate)?;
    run_fragmented_collection(context, &state, &mut candidate)?;
    let deferred_split_parent = if context.options.exhaustive {
        run_max_source_routes(context, &eligibility, &mut state, &mut candidate)?
    } else {
        None
    };
    run_complementary_no_split(context, &eligibility, &mut candidate)?;
    run_compact_balanced_tree(context, &eligibility, &mut candidate)?;

    // Standalone Complete work reaches this point with the historical Max seed
    // still driving every heuristic lineage. Compare the independently retained
    // complete Default endpoint only after those routes finish, so adding the
    // quality floor cannot redirect Max into a different rewritten-seed basin.
    if let Some(complete_default) = state.complete_default.take() {
        candidate.replace_if_smaller(complete_default);
    }
    if let Some(parent) = deferred_split_parent {
        run_deferred_source_max_split(context, deadlines, &parent, &mut candidate)?;
    }
    finish_terminal_searches(context, deadlines, candidate)
}

/// Build the comparison floor that some policies need before bounded routes.
///
/// A single scheduled PNG promises that max retains the complete ordinary
/// result. Prebuild compact, very large, one-block, or match-dense floors;
/// their exact Default route either is a cheap dependency or cannot reliably
/// finish inside the concurrent phase's reserved four-fifths. Medium
/// multi-block floors instead remain in the existing parallel phase, where
/// their completed ordinary candidate is still retained while max preserves
/// enough wall time for independent source routes.
fn build_guaranteed_floor(
    context: &RouteContext<'_>,
    state: &mut RouteState,
) -> Result<Option<Candidate>> {
    let RouteContext {
        source,
        options,
        default_floor,
        deadline,
        progress,
    } = *context;
    let prebuild_floor_first = options.exhaustive
        && match default_floor {
            DefaultFloor::CompleteThenBounded => {
                prebuild_bounded_floor(
                    nonempty_block_count(source.blocks),
                    source.identity.decoded_size,
                ) || source_run_match_count_exceeds(source.blocks, PROVEN_SUBMATCH_FULL_MATCH_LIMIT)
            }
            DefaultFloor::Shared
            | DefaultFloor::SharedExact
            | DefaultFloor::ApngDefault
            | DefaultFloor::ApngMax => true,
            DefaultFloor::Established => false,
            DefaultFloor::Complete | DefaultFloor::MandatoryComplete => false,
        };
    let step = prebuild_floor_first.then(|| progress.start("Normal comparison floor"));
    let floor = if default_floor == DefaultFloor::Established {
        Some(established_floor_candidate(source)?)
    } else if prebuild_floor_first {
        Some(match default_floor {
            DefaultFloor::CompleteThenBounded | DefaultFloor::SharedExact => {
                let floors = build_complete_default_floor_candidate(
                    source,
                    options,
                    progress,
                    context.timed(),
                )?;
                state.complete_default = Some(floors.complete);
                floors.max_seed
            }
            // APNG's initial planner and terminal transformations define its
            // exact Default endpoint. A replay-bounded seed is a different
            // lineage; a smaller seed does not dominate its terminal children.
            DefaultFloor::ApngMax => {
                state.complete_default = Some(build_complete_apng_default_floor_candidate(
                    source,
                    options,
                    progress,
                    context.timed(),
                )?);
                build_bounded_floor_candidate(source, options, &mut SearchStop::never())?
            }
            _ => build_bounded_floor_candidate(source, options, &mut deadline.hard_stop())?,
        })
    } else {
        None
    };
    if let Some(step) = step {
        let reported_floor = state.complete_default.as_ref().or(floor.as_ref());
        step.finish(context.report_optional(reported_floor));
    }
    Ok(floor)
}

/// Run the bounded comparison routes and retain each returned lineage.
fn run_bounded_phase(
    context: &RouteContext<'_>,
    eligibility: &RouteEligibility,
    guaranteed_floor: Option<Candidate>,
    state: &mut RouteState,
) -> Result<()> {
    let RouteContext {
        source,
        options,
        default_floor,
        deadline,
        progress,
    } = *context;
    let png_policy = if default_floor.uses_bounded_png_routes() && eligibility.parallel_routes {
        let floor_exposes_new_states = guaranteed_floor
            .as_ref()
            .is_some_and(|floor| floor_exposes_new_search_states(source.blocks, &floor.plans));
        bounded_png_max_policy(
            nonempty_block_count(source.blocks),
            floor_exposes_new_states,
            eligibility.deft4j,
            eligibility.narrow_source,
        )
    } else {
        BoundedPngMaxPolicy::default()
    };
    let bounded = options.exhaustive && default_floor.is_bounded();
    let step = bounded.then(|| progress.start("Bounded comparison routes"));
    let candidates = if bounded {
        // A one-block topology probe has already finished this exact floor.
        // Reuse it; multi-block streams build the same floor concurrently with
        // their independent source routes inside the bounded phase.
        match png_policy {
            BoundedPngMaxPolicy::GenericParallel => build_bounded_generic_max_candidates(
                source,
                options,
                deadline,
                progress,
                guaranteed_floor,
            )?,
            BoundedPngMaxPolicy::Standard | BoundedPngMaxPolicy::FloorExpansion => {
                let run_deft4j = eligibility.deft4j && deadline.can_start_route();
                let floor_expansion = png_policy == BoundedPngMaxPolicy::FloorExpansion;
                let run_source_max = eligibility.parallel_routes
                    && floor_expansion
                    && if default_floor.uses_bounded_png_routes() {
                        complete_png_parallel_source_max_work_class(source)
                    } else {
                        bounded_parallel_source_max_work_class(source)
                    };
                let routes = BoundedRoutes {
                    preserve_complete_default: default_floor == DefaultFloor::CompleteThenBounded,
                    seeded_max: floor_expansion,
                    deft4j: run_deft4j,
                    narrow: eligibility.narrow_source,
                    source_max: run_source_max,
                    proven_feedback: run_source_max && eligibility.compact_proven_feedback,
                    parallel: eligibility.parallel_routes,
                };
                build_bounded_phase_candidates(
                    source,
                    options,
                    routes,
                    deadline,
                    progress,
                    guaranteed_floor,
                )?
            }
        }
    } else {
        // Every policy that prebuilds a floor is bounded, and only bounded
        // Max consumes it.
        debug_assert!(guaranteed_floor.is_none());
        BoundedPhaseCandidates::default()
    };
    state.floor = candidates.floor;
    state.floor_seeded = candidates.floor_seeded;
    state.deft4j = candidates.deft4j;
    state.narrow = candidates.narrow;
    state.source_max = candidates.source_max;
    state.proven_feedback = candidates.proven_feedback;
    state.suppress_later_source_max = candidates.suppress_later_source_max;
    state.suppress_later_optional_routes = candidates.suppress_later_optional_routes;
    state.completed_compact_split_parent = candidates.completed_compact_split_parent;
    if let Some(step) = step {
        step.finish_phase();
        for (name, candidate) in [
            ("Normal floor", state.floor.as_ref()),
            ("Columbo floor-seeded", state.floor_seeded.as_ref()),
            ("deft4j-derived source", state.deft4j.as_ref()),
            ("No-split source", state.narrow.as_ref()),
            ("Columbo source max", state.source_max.as_ref()),
            ("Columbo proven-feedback", state.proven_feedback.as_ref()),
        ] {
            if let Some(candidate) = candidate {
                progress.candidate(name, context.report(candidate));
            }
        }
    }
    Ok(())
}

/// Price the compact proven-feedback sibling and merge it into the floor.
///
/// A compact one-block stream has one additional fixed point when proven
/// resegmentation feeds later table feedback before the normal endpoint
/// ordering. Price that bounded sibling before the general source-max graph
/// can consume the shared deadline. The completed normal floor remains an
/// independent fallback.
fn run_proven_feedback_floor(
    context: &RouteContext<'_>,
    eligibility: &RouteEligibility,
    state: &mut RouteState,
) -> Result<()> {
    let RouteContext {
        source,
        options,
        deadline,
        progress,
        ..
    } = *context;
    let run = eligibility.compact_proven_feedback
        && state.proven_feedback.is_none()
        && deadline.can_start_route();
    let step = run.then(|| progress.start("Columbo proven-feedback floor"));
    if run {
        state.proven_feedback =
            build_compact_proven_feedback_candidate(source, options, &mut deadline.hard_stop())?;
    }
    if let Some(step) = step {
        step.finish(context.report_optional(state.proven_feedback.as_ref()));
    }
    // The completed compact routes provide a direct scheduling signal for
    // source max. If proven-before-feedback supplies only a bit-level win,
    // continue that state order in the one expensive beam to seek the next
    // byte boundary. Once that bounded lineage already wins a byte, retain it
    // and spend the beam on complementary ordinary states instead. A tie also
    // selects ordinary states. Larger blocks use the integrated order
    // independently inside the stream planner.
    state.integrated_compact_source_max = state
        .proven_feedback
        .as_ref()
        .zip(state.floor.as_ref())
        .is_some_and(|(proven, normal)| {
            proven.data.len() == normal.data.len() && proven.bits < normal.bits
        });
    if let Some(proven_feedback) = state.proven_feedback.take() {
        replace_optional_if_smaller(&mut state.floor, proven_feedback);
    }
    Ok(())
}

/// Decide whether compact split applies and prepare its distinct parents.
///
/// Prepare the exact structural siblings before choosing which route gets the
/// remaining time. A weak direct gain admits compact-split inspection, but it
/// does not prove that any parent satisfies that route's bounded topology and
/// work model. Retaining these prepared seeds also avoids reparsing the same
/// candidates after the scheduling decision.
fn prepare_compact_split_seeds(context: &RouteContext<'_>, state: &mut RouteState) -> Result<()> {
    let source = context.source;
    let (decoded_limit, identity) = (source.decoded_limit, source.identity);
    state.weak_deft4j = context.default_floor.uses_bounded_png_routes()
        && state.deft4j.as_ref().is_some_and(|deft4j| {
            has_multiple_nonempty_blocks(source.blocks)
                && gain_is_below(
                    source.meaningful_bits,
                    deft4j.bits,
                    WEAK_DEFT4J_GAIN_BASIS_POINTS,
                )
        });
    if state.weak_deft4j {
        // Split pricing is not monotone in the parent stream's encoded size:
        // independently rewritten block topologies can have opposite local
        // ordering after new cuts are inserted. Preserve each distinct parent.
        let completed = state.completed_compact_split_parent.as_deref();
        let normal_parent = state
            .floor
            .as_ref()
            .filter(|candidate| !compact_split_parent_is_completed(candidate, completed));
        let seeded_parent = state.floor_seeded.as_ref().filter(|seeded| {
            !compact_split_parent_is_completed(seeded, completed)
                && normal_parent.map_or(true, |normal| normal.data != seeded.data)
        });
        let deft4j_parent = state.deft4j.as_ref().filter(|deft4j| {
            !compact_split_parent_is_completed(deft4j, completed)
                && [normal_parent, seeded_parent]
                    .into_iter()
                    .flatten()
                    .all(|seed| seed.data != deft4j.data)
        });
        let prepare = |parent: Option<&Candidate>| {
            parent
                .map(|candidate| {
                    prepare_compact_source_split_seed(candidate, decoded_limit, identity)
                })
                .transpose()
                .map(Option::flatten)
        };
        state.split_seeds = CompactSplitSeeds {
            normal: prepare(normal_parent)?,
            seeded: prepare(seeded_parent)?,
            deft4j: prepare(deft4j_parent)?,
        };
    }
    if let Some(floor) = &mut state.floor {
        // The later deft4j refinement needs only the encoded floor for its
        // strict comparison. Release transformed floor plans before it
        // reparses another complete candidate.
        floor.plans.clear();
    }
    Ok(())
}

/// Finish deterministic cleanup on a completed floor-seeded Max parent.
///
/// A completed floor-seeded max route may end at a different header/token
/// fixed point from source max. Finish the same bounded tree cleanup on that
/// exact parent before releasing it. This is deterministic finalization of an
/// already completed candidate, so it remains valid after the soft deadline
/// and cannot discard the parent when no balanced-tree improvement exists.
fn finish_floor_seeded_tree(
    context: &RouteContext<'_>,
    eligibility: &RouteEligibility,
    state: &mut RouteState,
) -> Result<()> {
    let RouteContext {
        source,
        options,
        deadline,
        ..
    } = *context;
    let (decoded_limit, identity) = (source.decoded_limit, source.identity);
    if eligibility.compact_tree {
        if let Some(seeded) = &mut state.floor_seeded {
            // A one-block source may become a multi-block floor after the max
            // descendant. Those new boundaries admit the same deterministic
            // split floor used for compact source lists, and the original
            // one-block routes cannot reconstruct that parent state.
            if let Some(split) = refine_with_compact_source_split_floor_until(
                seeded,
                options,
                decoded_limit,
                identity,
                &mut deadline.hard_stop(),
            )? {
                seeded.replace_if_smaller(split);
            }
            if let Some(mut tree) =
                refine_with_compact_balanced_tree_floor(seeded, options, decoded_limit, identity)?
            {
                if let Some(feedback) =
                    refine_with_compact_proven_feedback(&tree, options, decoded_limit, identity)?
                {
                    tree.replace_if_smaller(feedback);
                }
                seeded.replace_if_smaller(tree);
            }
        }
    }
    if let Some(seeded) = &mut state.floor_seeded {
        seeded.plans.clear();
    }
    Ok(())
}

/// Continue the strongest unfinished floor-seeded lineage.
///
/// Continue the strongest unfinished dependent lineage before starting
/// refinements from weaker parents. This is a score-ordered search rule, not a
/// size or corpus gate: the retained incumbent protects every other complete
/// result. It also avoids waiting for an independent source worker and then
/// spending the final allowance refining a candidate already behind the
/// floor-seeded endpoint.
fn continue_floor_seeded(
    context: &RouteContext<'_>,
    eligibility: &RouteEligibility,
    state: &mut RouteState,
) -> Result<()> {
    let RouteContext {
        source,
        options,
        default_floor,
        deadline,
        progress,
    } = *context;
    let (decoded_limit, identity) = (source.decoded_limit, source.identity);
    // An admitted compact split is an independent bounded sibling, but merely
    // being eligible does not predict that it will improve its parent. When
    // the parsed model permits route parallelism, overlap it with this
    // continuation so neither speculative route can starve the other. A
    // serial work class retains the material-gain priority rule below.
    let compact_split_pending = state.split_seeds.any();
    let overlap_compact_split = compact_split_pending && eligibility.parallel_routes;
    let continue_best = state.floor_seeded.as_ref().is_some_and(|seeded| {
        !seeded.max_planner_is_stable
            && state.floor.as_ref().is_some_and(|floor| {
                seeded.is_strictly_smaller_than(floor)
                    && floor_seeded_priority_with_structural_sibling(
                        floor.bits,
                        seeded.bits,
                        compact_split_pending,
                        overlap_compact_split,
                    )
            })
            && [
                state.deft4j.as_ref(),
                state.narrow.as_ref(),
                state.source_max.as_ref(),
            ]
            .into_iter()
            .flatten()
            .all(|other| !other.is_strictly_smaller_than(seeded))
    }) && deadline.can_start_route();
    // An encoded-size lead does not dominate the endpoint of a different
    // planner topology. In the bounded parallel work class, keep the
    // independent deft4j-derived refinement live beside the best floor-seeded
    // continuation. Otherwise a long fixed-point continuation can consume
    // every larger deadline without ever admitting the complementary route.
    // This uses the same at-most-three-worker envelope as the historical
    // source/deft4j/compact phase and does not add wall-clock budget.
    let overlap_deft4j = independent_deft4j_refinement_can_overlap(
        continue_best,
        default_floor.uses_bounded_png_routes(),
        eligibility.parallel_routes,
        state.weak_deft4j || state.deft4j.is_some(),
    );
    // APNG children have no serial time after a continuation that uses their
    // whole assigned window. Keep the independent original-source root live
    // beside that continuation; otherwise every larger allowance merely lets
    // the same dependent planner run longer and can never reach the omitted
    // basin. Standalone PNG keeps its existing bounded route envelope here.
    let overlap_source_max = continue_best
        && default_floor == DefaultFloor::ApngMax
        && eligibility.parallel_routes
        && state.source_max.is_none()
        && !state.suppress_later_source_max
        && deadline.can_start_route();
    let floor_seeded_step =
        continue_best.then(|| progress.start("Columbo floor-seeded continuation"));
    let split_step = (continue_best && overlap_compact_split)
        .then(|| progress.start("Columbo compact split floor"));
    let deft4j_step = overlap_deft4j.then(|| progress.start("deft4j-derived refinement"));
    let mut floor_seeded_changed = false;
    if continue_best {
        let refined = if overlap_source_max {
            continue_beside_source_max(context, state)?
        } else if overlap_deft4j {
            continue_beside_deft4j_refinement(context, state, overlap_compact_split)?
        } else if overlap_compact_split {
            continue_beside_compact_split(context, state)?
        } else {
            let seeded = floor_seeded_parent(state);
            Some(refine_with_max_planner(
                seeded,
                options,
                decoded_limit,
                identity,
                &mut deadline.hard_stop(),
            )?)
        };
        if let Some(refined) = refined {
            let seeded = state
                .floor_seeded
                .as_mut()
                .expect("continuation requires a floor-seeded candidate");
            floor_seeded_changed = seeded.replace_if_smaller(refined);
        }
    }
    if let Some(step) = floor_seeded_step {
        step.finish(context.report_optional(state.floor_seeded.as_ref()));
    }
    if let Some(step) = split_step {
        step.finish(context.report_optional(state.compact_split.as_ref()));
    }
    if let Some(step) = deft4j_step {
        step.finish(context.report_optional(state.deft4j.as_ref()));
    }
    if floor_seeded_changed && state.weak_deft4j {
        // Continuation can emit a new topology. Refresh only that changed
        // parent; the normal and direct deft4j seeds above remain exact and
        // must not be reparsed. Exact identity with either seed proves that
        // its structural work is already represented.
        let seeds = &state.split_seeds;
        let completed = state.completed_compact_split_parent.as_deref();
        let refreshed = state
            .floor_seeded
            .as_ref()
            .filter(|seeded| {
                !compact_split_parent_is_completed(seeded, completed)
                    && [seeds.normal.as_ref(), seeds.deft4j.as_ref()]
                        .into_iter()
                        .flatten()
                        .all(|seed| seed.data != seeded.data)
            })
            .map(|seeded| prepare_compact_source_split_seed(seeded, decoded_limit, identity))
            .transpose()?
            .flatten();
        state.split_seeds.seeded = refreshed;
    }
    Ok(())
}

fn floor_seeded_parent(state: &RouteState) -> &Candidate {
    state
        .floor_seeded
        .as_ref()
        .expect("continuation requires a floor-seeded candidate")
}

/// Continue the floor-seeded parent beside an original-source max worker.
fn continue_beside_source_max(
    context: &RouteContext<'_>,
    state: &mut RouteState,
) -> Result<Option<Candidate>> {
    let RouteContext {
        source,
        options,
        deadline,
        progress,
        ..
    } = *context;
    let (decoded_limit, identity) = (source.decoded_limit, source.identity);
    let seeded = floor_seeded_parent(state);
    let integrated_compact_source_max = state.integrated_compact_source_max;
    let (refined, source_max) =
        thread::scope(|scope| -> Result<(Option<Candidate>, Option<Candidate>)> {
            let source_worker =
                spawn_route(scope, "columbo-source-max-continuation", deadline, || {
                    build_source_max_candidate(
                        source,
                        options,
                        progress,
                        deadline,
                        integrated_compact_source_max,
                        &mut deadline.hard_stop(),
                    )
                });
            let Some(source_worker) = source_worker else {
                // Thread exhaustion retains the prior score-ordered
                // continuation. A later serial source route remains eligible
                // if that continuation finishes in time.
                let refined = refine_with_max_planner(
                    seeded,
                    options,
                    decoded_limit,
                    identity,
                    &mut deadline.hard_stop(),
                )?;
                return Ok((Some(refined), None));
            };
            let refined = refine_with_max_planner(
                seeded,
                options,
                decoded_limit,
                identity,
                &mut deadline.hard_stop(),
            );
            if refined.is_err() {
                deadline.cancel_routes();
            }
            let source_max = join_route(source_worker)?;
            Ok((Some(refined?), Some(source_max)))
        })?;
    if let Some(source_max) = source_max {
        state.source_max = Some(source_max);
        state.suppress_later_source_max = true;
    }
    Ok(refined)
}

/// Continue the floor-seeded parent beside the deft4j-derived refinement,
/// and beside compact split when that sibling can also overlap.
fn continue_beside_deft4j_refinement(
    context: &RouteContext<'_>,
    state: &mut RouteState,
    overlap_compact_split: bool,
) -> Result<Option<Candidate>> {
    let RouteContext {
        source,
        options,
        deadline,
        ..
    } = *context;
    let (decoded_limit, identity) = (source.decoded_limit, source.identity);
    let RouteState {
        floor,
        floor_seeded,
        deft4j,
        narrow,
        split_seeds,
        weak_deft4j,
        ..
    } = state;
    let seeded = floor_seeded
        .as_ref()
        .expect("continuation requires a floor-seeded candidate");
    let (floor, narrow, split_seeds, weak_deft4j) =
        (floor.as_ref(), narrow.as_ref(), &*split_seeds, *weak_deft4j);
    let (refined, split, refinement_completed) = thread::scope(
        |scope| -> Result<(Option<Candidate>, Option<Candidate>, bool)> {
            let refinement_worker =
                spawn_route(scope, "columbo-deft4j-continuation", deadline, || {
                    refine_bounded_deft4j_lineage(
                        source,
                        options,
                        decoded_limit,
                        identity,
                        deadline,
                        floor,
                        narrow,
                        weak_deft4j,
                        false,
                        deft4j,
                    )
                });
            let Some(refinement_worker) = refinement_worker else {
                // Retain both complete parents and let the ordinary serial
                // phase below run the independent refinement.
                return Ok((None, None, false));
            };
            let split_worker = overlap_compact_split
                .then(|| {
                    spawn_route(
                        scope,
                        "columbo-compact-split-continuation",
                        deadline,
                        || split_seeds.build(context),
                    )
                })
                .flatten();
            let refined = refine_with_max_planner(
                seeded,
                options,
                decoded_limit,
                identity,
                &mut deadline.hard_stop(),
            );
            if refined.is_err() {
                deadline.cancel_routes();
            }
            join_route(refinement_worker)?;
            let split = match split_worker {
                Some(worker) => join_route(worker)?,
                // A failed split-worker spawn retains the bounded synchronous
                // fallback used by the prior schedule.
                None if overlap_compact_split => split_seeds.build(context)?,
                None => None,
            };
            Ok((Some(refined?), split, true))
        },
    )?;
    state.deft4j_refinement_completed = refinement_completed;
    if refinement_completed && overlap_compact_split {
        state.compact_split_attempted = true;
        state.split_seeds.clear();
    }
    if let Some(split) = split {
        replace_optional_if_smaller(&mut state.compact_split, split);
    }
    Ok(refined)
}

/// Continue the floor-seeded parent beside the compact split sibling.
fn continue_beside_compact_split(
    context: &RouteContext<'_>,
    state: &mut RouteState,
) -> Result<Option<Candidate>> {
    let RouteContext {
        source,
        options,
        deadline,
        ..
    } = *context;
    let (decoded_limit, identity) = (source.decoded_limit, source.identity);
    let seeded = floor_seeded_parent(state);
    let split_seeds = &state.split_seeds;
    let (refined, split) =
        thread::scope(|scope| -> Result<(Option<Candidate>, Option<Candidate>)> {
            let split_worker = spawn_route(
                scope,
                "columbo-compact-split-continuation",
                deadline,
                || split_seeds.build(context),
            );
            let Some(split_worker) = split_worker else {
                // Thread exhaustion must not discard the independent
                // structural sibling. Finish it on this thread and retain the
                // complete seeded parent as the fallback.
                return split_seeds.build(context).map(|split| (None, split));
            };
            let refined = refine_with_max_planner(
                seeded,
                options,
                decoded_limit,
                identity,
                &mut deadline.hard_stop(),
            );
            if refined.is_err() {
                deadline.cancel_routes();
            }
            let split = join_route(split_worker)?;
            Ok((Some(refined?), split))
        })?;
    state.compact_split_attempted = true;
    state.split_seeds.clear();
    if let Some(split) = split {
        replace_optional_if_smaller(&mut state.compact_split, split);
    }
    Ok(refined)
}

/// Run the bounded follow-up routes, then finish compact split serially if no
/// concurrent route attempted it.
fn run_bounded_refinement(
    context: &RouteContext<'_>,
    eligibility: &RouteEligibility,
    state: &mut RouteState,
) -> Result<()> {
    let RouteContext {
        source,
        options,
        default_floor,
        deadline,
        progress,
    } = *context;
    let (decoded_limit, identity) = (source.decoded_limit, source.identity);
    let run_bounded_refinement = default_floor.is_bounded() && deadline.can_start_route();
    let compact_split_work_possible = run_bounded_refinement
        && (state.split_seeds.any()
            // Refinement can expose a distinct eligible parent even when none
            // of the early encodings qualify. Use the same bounded source work
            // model that admits that dependency; larger ineligible streams
            // should not display an idle compact-split route.
            || (state.weak_deft4j && compact_dependent_deft4j_work_class(source)))
        // If no early split ran, the hard-deadline fallback below may still
        // finish one parent after the soft route window closes.
        || (state.weak_deft4j && !state.compact_split_attempted && !deadline.expired());
    let compact_split_step =
        compact_split_work_possible.then(|| progress.start("Columbo compact split floor"));
    // The direct no-split walk is not idempotent across an emitted token or
    // block rewrite: its new parent can expose another cumulative-pruning or
    // adjacent-merge choice. Continue that dependency once when it is a
    // non-dominated complete result. This is score ordered rather than tied to
    // a corpus shape, and source max remains available later if time remains.
    // Prefer the dependent continuation to a simultaneous source-max worker so
    // the bounded phase retains its existing three-worker memory envelope.
    let continue_best_narrow = run_bounded_refinement
        && state.narrow.as_ref().is_some_and(|narrow| {
            changed_narrow_parent_should_continue(
                candidate_exposes_new_parent(narrow, source),
                narrow,
                source,
            )
        })
        && deadline.can_start_route();
    let narrow_step = continue_best_narrow.then(|| progress.start("Columbo no-split continuation"));
    // Refinement needs only the encoded streams. Release retained route token
    // plans before reparsing them so the models do not overlap at peak memory.
    for candidate in [&mut state.deft4j, &mut state.narrow, &mut state.source_max]
        .into_iter()
        .flatten()
    {
        candidate.plans.clear();
    }
    let refinement_step = (run_bounded_refinement
        && !state.deft4j_refinement_completed
        && (state.weak_deft4j || state.deft4j.is_some()))
    .then(|| progress.start("deft4j-derived refinement"));
    if run_bounded_refinement {
        let routes = FollowUpRoutes {
            source_max: options.exhaustive
                && default_floor.allows_parallel_source_follow_up()
                && eligibility.parallel_routes
                && state.source_max.is_none()
                && !state.suppress_later_source_max
                && !continue_best_narrow
                && deadline.can_start_route(),
            narrow: continue_best_narrow,
            compact_split: state.split_seeds.any(),
        };
        let follow_up = if routes.source_max || routes.narrow || routes.compact_split {
            run_concurrent_follow_up(context, state, routes)?
        } else {
            if !state.deft4j_refinement_completed {
                refine_bounded_deft4j_lineage(
                    source,
                    options,
                    decoded_limit,
                    identity,
                    deadline,
                    state.floor.as_ref(),
                    state.narrow.as_ref(),
                    state.weak_deft4j,
                    false,
                    &mut state.deft4j,
                )?;
            }
            BoundedFollowUpCandidates::default()
        };
        state.compact_split_attempted |= follow_up.attempted_compact_split;
        if let Some(compact_split) = follow_up.compact_split {
            replace_optional_if_smaller(&mut state.compact_split, compact_split);
        }
        if let Some(source_max) = follow_up.source_max {
            state.source_max = Some(source_max);
            state.suppress_later_source_max = true;
        }
        if let Some(continued) = follow_up.narrow {
            replace_optional_if_smaller(&mut state.narrow, continued);
        }
    }
    if let Some(step) = narrow_step {
        step.finish(context.report_optional(state.narrow.as_ref()));
    }
    if let Some(step) = refinement_step {
        step.finish(context.report_optional(state.deft4j.as_ref()));
    }
    // This structural cleanup normally runs in the deft4j lineage beside
    // source max. If that concurrent phase could not run, finish it serially
    // only while the file's critical deadline remains. The deadline-aware
    // variant retains the completed parent if an active split trial runs out
    // of grace instead of beginning unbounded work after the hard stop.
    if state.weak_deft4j && !state.compact_split_attempted && !deadline.expired() {
        let completed = state.completed_compact_split_parent.as_deref();
        state.compact_split = match state.deft4j.as_ref() {
            Some(deft4j) if !compact_split_parent_is_completed(deft4j, completed) => {
                refine_with_compact_source_split_floor_until(
                    deft4j,
                    options,
                    decoded_limit,
                    identity,
                    &mut deadline.hard_stop(),
                )?
            }
            None => None,
            Some(_) => None,
        };
    }
    if let Some(step) = compact_split_step {
        step.finish(context.report_optional(state.compact_split.as_ref()));
    }
    if let Some(split) = state.compact_split.take() {
        replace_optional_if_smaller(&mut state.deft4j, split);
    }
    Ok(())
}

/// Follow-up workers admitted beside the deft4j-derived refinement.
#[derive(Clone, Copy)]
struct FollowUpRoutes {
    source_max: bool,
    narrow: bool,
    compact_split: bool,
}

/// Refine the deft4j lineage on this thread while admitted siblings run on
/// scoped workers. Every started worker joins before any error is returned.
fn run_concurrent_follow_up(
    context: &RouteContext<'_>,
    state: &mut RouteState,
    routes: FollowUpRoutes,
) -> Result<BoundedFollowUpCandidates> {
    let RouteContext {
        source,
        options,
        deadline,
        progress,
        ..
    } = *context;
    let (decoded_limit, identity) = (source.decoded_limit, source.identity);
    let RouteState {
        floor,
        deft4j,
        narrow,
        split_seeds,
        completed_compact_split_parent,
        weak_deft4j,
        integrated_compact_source_max,
        deft4j_refinement_completed,
        ..
    } = state;
    let (floor, narrow, split_seeds) = (floor.as_ref(), narrow.as_ref(), &*split_seeds);
    let completed_compact_split_parent = completed_compact_split_parent.as_deref();
    let (weak_deft4j, integrated_compact_source_max, deft4j_refinement_completed) = (
        *weak_deft4j,
        *integrated_compact_source_max,
        *deft4j_refinement_completed,
    );
    thread::scope(|scope| -> Result<BoundedFollowUpCandidates> {
        let source_worker = routes
            .source_max
            .then(|| {
                spawn_route(scope, "columbo-source-max-follow-up", deadline, || {
                    build_source_max_candidate(
                        source,
                        options,
                        progress,
                        deadline,
                        integrated_compact_source_max,
                        &mut deadline.hard_stop(),
                    )
                })
            })
            .flatten();
        let compact_worker = routes
            .compact_split
            .then(|| {
                spawn_route(scope, "columbo-compact-split-lineages", deadline, || {
                    split_seeds.build(context)
                })
            })
            .flatten();
        let narrow_worker = routes
            .narrow
            .then(|| {
                spawn_route(scope, "columbo-no-split-continuation", deadline, || {
                    continue_no_split(context, narrow)
                })
            })
            .flatten();
        let refinement = if deft4j_refinement_completed {
            Ok(())
        } else {
            refine_bounded_deft4j_lineage(
                source,
                options,
                decoded_limit,
                identity,
                deadline,
                floor,
                narrow,
                weak_deft4j,
                routes.compact_split,
                deft4j,
            )
        };
        if refinement.is_err() {
            deadline.cancel_routes();
        }
        // A genuinely different refined topology is another split parent.
        // Price it while source max is still running when soft time remains,
        // or unconditionally when it strictly improves every early parent.
        // Exact encoded identity proves duplicate work.
        let dependent_split = if refinement.is_ok() && weak_deft4j {
            deft4j.as_ref().and_then(|deft4j| {
                let duplicates_early_seed = split_seeds
                    .as_refs()
                    .into_iter()
                    .flatten()
                    .any(|seed| seed.data == deft4j.data)
                    || compact_split_parent_is_completed(deft4j, completed_compact_split_parent);
                let improves_early_seeds =
                    split_seeds.as_refs().into_iter().flatten().all(|seed| {
                        is_strictly_better(
                            deft4j.data.len(),
                            deft4j.bits,
                            seed.data.len(),
                            seed.bits,
                        )
                    });
                (!duplicates_early_seed && (improves_early_seeds || deadline.can_start_route()))
                    .then(|| {
                        refine_with_compact_source_split_floor(
                            deft4j,
                            options,
                            decoded_limit,
                            identity,
                        )
                    })
            })
        } else {
            None
        };
        let source_max = match source_worker {
            Some(worker) => Some(join_route(worker)?),
            None => None,
        };
        let narrow_continuation = match narrow_worker {
            Some(worker) => join_route(worker),
            None if routes.narrow => continue_no_split(context, narrow),
            None => Ok(None),
        }?;
        let attempted_compact_split = routes.compact_split || dependent_split.is_some();
        // A failed worker spawn falls back to the same bounded pass on this
        // thread while source max is still joined.
        let early_split = match compact_worker {
            Some(worker) => join_route(worker),
            None => split_seeds.build(context),
        };
        refinement?;
        let mut compact_split = early_split?;
        if let Some(result) = dependent_split {
            if let Some(candidate) = result? {
                replace_optional_if_smaller(&mut compact_split, candidate);
            }
        }
        Ok(BoundedFollowUpCandidates {
            source_max,
            attempted_compact_split,
            compact_split,
            narrow: narrow_continuation,
        })
    })
}

/// Continue a changed no-split parent once with the same no-split route.
fn continue_no_split(
    context: &RouteContext<'_>,
    narrow: Option<&Candidate>,
) -> Result<Option<Candidate>> {
    let narrow = narrow.expect("scheduled no-split continuation");
    let deadline = context.deadline;
    let mut route_stop = deadline.hard_stop();
    let mut refinement_stop = deadline.hard_stop();
    refine_with_no_split_route(
        narrow,
        context.options,
        context.source.decoded_limit,
        context.source.identity,
        &mut route_stop,
        &mut refinement_stop,
    )
}

/// Close a changed no-split lineage only when another candidate would
/// otherwise discard it, then fold it into the deft4j lineage.
fn close_no_split_lineage(context: &RouteContext<'_>, state: &mut RouteState) -> Result<()> {
    let RouteContext {
        source,
        options,
        progress,
        ..
    } = *context;
    if let Some(deft4j) = &mut state.deft4j {
        // Keep only the encoded incumbent for comparison and later routes;
        // refinement can otherwise retain another expanded token graph.
        deft4j.plans.clear();
    }
    if let Some(mut narrow) = state.narrow.take() {
        // A changed no-split topology can lose the immediate encoded-size
        // comparison yet win after the bounded terminal tree closure. Close
        // that branch only when another completed candidate would otherwise
        // discard it; a no-split winner receives the same work once at Max's
        // ordinary terminal stage.
        let narrow_would_be_retained = [
            state.floor.as_ref(),
            state.floor_seeded.as_ref(),
            state.deft4j.as_ref(),
            state.source_max.as_ref(),
            state.complete_default.as_ref(),
        ]
        .into_iter()
        .flatten()
        .all(|other| narrow.is_strictly_smaller_than(other));
        if candidate_exposes_new_parent(&narrow, source) && !narrow_would_be_retained {
            narrow = improve_with_terminal_tree_floors(
                source,
                options,
                context.timed(),
                progress,
                narrow,
            )?;
        }
        replace_optional_if_smaller(&mut state.deft4j, narrow);
    }
    Ok(())
}

/// Select the incumbent every later route must strictly improve.
///
/// Bounded max routes deliberately retain their historical ordinary seed: a
/// smaller complete Default floor can occupy a different search basin.
/// Compare that independent floor only after those descendants finish, so max
/// gets both the established Default result and its original routes without
/// recomputing the shared base candidate.
fn establish_comparison_floor(
    context: &RouteContext<'_>,
    state: &mut RouteState,
) -> Result<Candidate> {
    let RouteContext {
        source,
        options,
        default_floor,
        deadline,
        progress,
    } = *context;
    if let Some(complete_default) = state.complete_default.take() {
        replace_optional_if_smaller(&mut state.floor, complete_default);
    }

    let initial_step = state.floor.is_none().then(|| {
        progress.start(if options.exhaustive {
            "Normal comparison floor"
        } else {
            "Normal route"
        })
    });
    let mut candidate = if let Some(floor) = state.floor.take() {
        floor
    } else if options.exhaustive {
        // Max mode finishes the genuine normal-mode route first. Its best
        // complete candidate remains the comparison floor even when the Max
        // deadline has already curtailed optional search.
        match default_floor {
            DefaultFloor::Complete => {
                let floors = build_complete_default_floor_candidate(
                    source,
                    options,
                    progress,
                    context.timed(),
                )?;
                state.complete_default = Some(floors.complete);
                floors.max_seed
            }
            DefaultFloor::MandatoryComplete => build_candidate(
                source,
                options,
                DEFAULT_RAW_REPLAY_LIMIT,
                &mut SearchStop::never(),
            )?,
            DefaultFloor::CompleteThenBounded
            | DefaultFloor::Shared
            | DefaultFloor::SharedExact
            | DefaultFloor::ApngDefault
            | DefaultFloor::ApngMax
            | DefaultFloor::Established => {
                build_bounded_floor_candidate(source, options, &mut deadline.hard_stop())?
            }
        }
    } else if default_floor == DefaultFloor::ApngDefault {
        // An APNG image stream is one part of a larger file-level Default run.
        // Keep its full initial planner, but leave repeated replay and the
        // independent endpoint-proven lineage to Max. Applying those additive
        // routes to every frame made Default scale with route count rather
        // than useful savings.
        build_apng_default_candidate(source, options, &mut deadline.hard_stop())?
    } else if default_floor == DefaultFloor::MandatoryComplete {
        build_candidate(
            source,
            options,
            DEFAULT_RAW_REPLAY_LIMIT,
            &mut SearchStop::never(),
        )?
    } else {
        build_candidate(
            source,
            options,
            DEFAULT_RAW_REPLAY_LIMIT,
            &mut deadline.hard_stop(),
        )?
    };
    if let Some(step) = initial_step {
        step.finish(Some(context.report(&candidate)));
    }
    // The APNG-specific fast shared floor is complete and validated, but
    // repeating optional compact feedback siblings for every frame can turn
    // individually bounded routes into an unbounded file-wide Default cost.
    // Standalone, metadata, and other shared callers retain their existing
    // fixed points; Max covers the broader APNG feedback families.
    if !options.exhaustive && default_floor != DefaultFloor::ApngDefault {
        let floor_work = if default_floor == DefaultFloor::MandatoryComplete {
            DefaultFloorWork::Mandatory
        } else {
            context.timed()
        };
        candidate =
            improve_default_floor_with_feedback(source, options, floor_work, progress, candidate)?;
    }
    Ok(candidate)
}

/// Close a different topology's losing branch before comparing it.
///
/// Encoded size does not dominate a different token/tree topology before its
/// bounded tree-only closure. Close only a losing branch here: a winning
/// contender receives the same terminal work once at the end of Max, so this
/// preserves the independent search basin without duplicating its
/// finalization on the common path.
fn close_losing_topology(
    context: &RouteContext<'_>,
    incumbent: &Candidate,
    contender: Candidate,
) -> Result<Candidate> {
    if contender.is_strictly_smaller_than(incumbent) {
        return Ok(contender);
    }
    improve_with_terminal_tree_floors(
        context.source,
        context.options,
        context.timed(),
        context.progress,
        contender,
    )
}

/// Compare the floor-seeded and deft4j-derived topologies with the incumbent.
fn merge_independent_topologies(
    context: &RouteContext<'_>,
    eligibility: &RouteEligibility,
    state: &mut RouteState,
    candidate: &mut Candidate,
) -> Result<()> {
    let RouteContext {
        source,
        options,
        default_floor,
        deadline,
        progress,
    } = *context;
    if eligibility.deft4j && default_floor == DefaultFloor::Complete && deadline.can_start_route() {
        let deft4j_step = progress.start("deft4j-derived source route");
        // Complete streams run no bounded phase, so this is the first deft4j
        // candidate for the lineage.
        state.deft4j = build_deft4j_source_candidate(source, options, &mut deadline.hard_stop())?;
        deft4j_step.finish(context.report_optional(state.deft4j.as_ref()));
    }

    if let Some(seeded) = state.floor_seeded.take() {
        // The retained Default floor may be immediately smaller while a
        // distinct floor-seeded topology reaches a better terminal tree fixed
        // point.
        let seeded = close_losing_topology(context, candidate, seeded)?;
        candidate.replace_if_smaller(seeded);
    }

    if let Some(mut deft4j) = state.deft4j.take() {
        deft4j.plans.clear();
        // The deft4j-derived source graph is another independent topology;
        // encoded-size dominance is sound only after its bounded tree-only
        // closure has been priced.
        let deft4j = close_losing_topology(context, candidate, deft4j)?;
        candidate.replace_if_smaller(deft4j);
    }
    Ok(())
}

/// Run the fragmented collection route as an additive Max sibling.
///
/// A 4,096-token collection can start slightly larger but converge to a better
/// fragmented-stream layout after strict replays. It is additive to the
/// normal-mode comparison floor, and starts only while the soft deadline still
/// permits a new independent route.
fn run_fragmented_collection(
    context: &RouteContext<'_>,
    state: &RouteState,
    candidate: &mut Candidate,
) -> Result<()> {
    let RouteContext {
        source,
        options,
        deadline,
        progress,
        ..
    } = *context;
    let run =
        options.exhaustive && !state.suppress_later_optional_routes && deadline.can_start_route();
    let step = run.then(|| progress.start("Columbo fragmented collection"));
    let mut fragmented = if run {
        fragmented_collect_seed(source.blocks, 0, options)
            .map(|plans| {
                let mut replay_options = options.clone();
                replay_options.exhaustive = false;
                build_candidate_from_plans(
                    source,
                    plans,
                    &replay_options,
                    MAX_RAW_REPLAY_LIMIT,
                    ReplayPlanner::Fragmented,
                    &mut deadline.hard_stop(),
                )
                .map(|candidate| candidate.named("Columbo fragmented collection"))
            })
            .transpose()?
    } else {
        None
    };
    if let Some(step) = step {
        step.finish(context.report_optional(fragmented.as_ref()));
    }
    if let Some(fragmented) = &mut fragmented {
        fragmented.plans.clear();
    }
    if let Some(fragmented) = fragmented {
        candidate.replace_if_smaller(fragmented);
    }
    Ok(())
}

/// Run original-source max and the rewritten-seed refinement.
///
/// Returns a compact source-max parent whose structural split is deferred
/// until every ordinary timed route has finished.
fn run_max_source_routes(
    context: &RouteContext<'_>,
    eligibility: &RouteEligibility,
    state: &mut RouteState,
    candidate: &mut Candidate,
) -> Result<Option<Candidate>> {
    // The encoded floor is all we need for comparison. Releasing its copied
    // tokens before max search keeps peak memory predictable.
    candidate.plans.clear();
    let (deferred_split_parent, source_max_stabilized_incumbent) =
        merge_source_max(context, eligibility, state, candidate)?;
    // A winning source restart can own another full plan graph. Only its
    // encoded bytes are needed as the optional replay seed.
    candidate.plans.clear();
    refine_rewritten_seed(context, state, candidate, source_max_stabilized_incumbent)?;
    Ok(deferred_split_parent)
}

/// Compare the original-source max lineage with the incumbent.
///
/// Generic max also explores source-boundary and table families outside
/// deft4j's source-ordered graph. Run it before a rewritten seed can spend the
/// remainder on one large merged block. Returns the deferred split parent and
/// whether source max proved the incumbent's exact encoding stable.
fn merge_source_max(
    context: &RouteContext<'_>,
    eligibility: &RouteEligibility,
    state: &mut RouteState,
    candidate: &mut Candidate,
) -> Result<(Option<Candidate>, bool)> {
    let RouteContext {
        source,
        options,
        default_floor,
        deadline,
        progress,
    } = *context;
    let (decoded_limit, identity) = (source.decoded_limit, source.identity);
    let source_max = if let Some(max_candidate) = state.source_max.take() {
        Some(max_candidate)
    } else {
        let run_source_max = !state.suppress_later_source_max && deadline.can_start_route();
        if run_source_max {
            Some(build_source_max_candidate(
                source,
                options,
                progress,
                deadline,
                state.integrated_compact_source_max,
                &mut deadline.hard_stop(),
            )?)
        } else {
            None
        }
    };
    let Some(mut max_candidate) = source_max else {
        return Ok((None, false));
    };
    // Preserve only a compact encoded parent for deferred structural
    // finalization. Running that work here can consume live time from later
    // Max siblings; postponing it keeps the established route schedule
    // unchanged. The exact-parent check avoids repeating a split lineage
    // already completed during the bounded phase.
    let mut deferred_split_parent = None;
    if default_floor.owns_terminal_stream_time()
        && max_candidate.data.len() <= COMPACT_SPLIT_FLOOR_MAX_COMPRESSED
        && !compact_split_parent_is_completed(
            &max_candidate,
            state.completed_compact_split_parent.as_deref(),
        )
    {
        let mut parent = max_candidate.clone();
        parent.plans.clear();
        parent.block_report = None;
        deferred_split_parent = Some(parent);
    }

    // A locally smaller proven-feedback endpoint can hide the bounded
    // balanced-tree header win reachable from source max. Finish that cheap
    // lineage-specific cleanup before comparing complete streams.
    if eligibility.compact_tree {
        let tree_step = progress.start("Columbo source-max balanced-tree floor");
        let mut tree = refine_with_compact_balanced_tree_floor(
            &max_candidate,
            options,
            decoded_limit,
            identity,
        )?;
        if let Some(candidate) = tree.as_mut() {
            if let Some(feedback) =
                refine_with_compact_proven_feedback(candidate, options, decoded_limit, identity)?
            {
                candidate.replace_if_smaller(feedback);
            }
        }
        tree_step.finish(context.report_optional(tree.as_ref()));
        if let Some(tree) = tree {
            max_candidate.replace_if_smaller(tree);
        }
    }
    // In particular, a newly retained Default floor can be smaller than this
    // source-max parent while the latter still reaches the best payload-tree
    // fixed point.
    let max_candidate = close_losing_topology(context, candidate, max_candidate)?;
    let stabilized = candidate.is_encoding_stabilized_by(&max_candidate);
    candidate.replace_if_smaller(max_candidate);
    Ok((deferred_split_parent, stabilized))
}

/// Retain one additive Max pass from the selected rewritten seed.
///
/// Rewritten match choices and boundaries can expose later max
/// transformations, so retain one additive seeded pass after both
/// source-shaped routes. Its incumbent remains available if this final route
/// times out or fails to improve it.
fn refine_rewritten_seed(
    context: &RouteContext<'_>,
    state: &RouteState,
    candidate: &mut Candidate,
    source_max_stabilized_incumbent: bool,
) -> Result<()> {
    let RouteContext {
        source,
        options,
        deadline,
        progress,
        ..
    } = *context;
    let seed_selected = options.strict || candidate.is_strictly_smaller_than_source(source);
    // `build_candidate` has already run the same max planner on every accepted
    // rewrite until it either stopped improving or hit its replay cap. If
    // source max proved these exact bytes stable, reparsing them here can only
    // repeat that final no-improvement round.
    let max_seed_is_stable = candidate.max_planner_is_stable || source_max_stabilized_incumbent;
    if seed_selected && max_seed_is_stable {
        progress.skipped(
            "Columbo rewritten-seed refinement",
            "exact max-planner fixed point already established",
        );
    }
    let run_seeded = seed_selected
        && !max_seed_is_stable
        && !state.suppress_later_optional_routes
        && deadline.can_start_route();
    if !run_seeded {
        return Ok(());
    }
    let step = progress.start("Columbo rewritten-seed refinement");
    let seeded = refine_with_max_planner(
        candidate,
        options,
        source.decoded_limit,
        source.identity,
        &mut deadline.hard_stop(),
    )?;
    step.finish(Some(context.report(&seeded)));
    candidate.replace_if_smaller(seeded);
    Ok(())
}

/// Price the complementary no-split pruning policy as its own topology.
///
/// The topology-selected no-split lineage owns the early bounded window: short
/// lists keep individual pruning, while long lists first ensure that
/// cumulative search reaches every source block. With time still available,
/// retain the complementary pruning policy as its own topology rather than
/// folding a locally smaller block choice into (and potentially redirecting)
/// the established no-split candidate.
fn run_complementary_no_split(
    context: &RouteContext<'_>,
    eligibility: &RouteEligibility,
    candidate: &mut Candidate,
) -> Result<()> {
    let RouteContext {
        source,
        options,
        default_floor,
        deadline,
        progress,
    } = *context;
    let run = options.exhaustive
        && default_floor.uses_bounded_png_routes()
        && eligibility.narrow_source
        && deadline.can_start_route();
    if !run {
        return Ok(());
    }
    let step = progress.start("Columbo complementary no-split route");
    let mut route_stop = deadline.hard_stop();
    let mut refinement_stop = deadline.hard_stop();
    let complementary = build_complementary_narrow_source_candidate(
        source,
        options,
        &mut route_stop,
        &mut refinement_stop,
    )?;
    let Some(contender) = complementary else {
        step.finish(None);
        return Ok(());
    };
    let contender = close_losing_topology(context, candidate, contender)?;
    step.finish(Some(context.report(&contender)));
    candidate.replace_if_smaller(contender);
    Ok(())
}

fn run_compact_balanced_tree(
    context: &RouteContext<'_>,
    eligibility: &RouteEligibility,
    candidate: &mut Candidate,
) -> Result<()> {
    if !(eligibility.compact_tree && context.deadline.can_start_route()) {
        return Ok(());
    }
    let tree_step = context
        .progress
        .start("Columbo compact balanced-tree floor");
    let tree = refine_with_compact_balanced_tree_floor(
        candidate,
        context.options,
        context.source.decoded_limit,
        context.source.identity,
    )?;
    tree_step.finish(context.report_optional(tree.as_ref()));
    if let Some(tree) = tree {
        candidate.replace_if_smaller(tree);
    }
    Ok(())
}

/// Price the saved compact source-max parent's structural split.
///
/// Source max can finish on a different block/tree endpoint from the
/// deft4j-derived parent priced earlier. Immediate encoded size does not
/// dominate that dependency: one child split can give locally distinct payload
/// regimes separate trees. Price this saved compact parent only after all
/// ordinary timed routes have finished. The `_until` variant uses any
/// remaining allowance or its deterministic one-block hard-deadline rescue,
/// while the incumbent remains available on failure or non-win.
fn run_deferred_source_max_split(
    context: &RouteContext<'_>,
    deadlines: &StreamDeadlines,
    parent: &Candidate,
    candidate: &mut Candidate,
) -> Result<()> {
    let deadline = context.deadline;
    let split_step = context
        .progress
        .start("Columbo source-max compact split floor");
    // A primary phase yield is not a file timeout. Preserve its cheap coarse
    // rescue only while the terminal share is still available; do not let
    // exhaustive split pricing spend that reserved share.
    let mut split_stop =
        if deadlines.reserve_terminal && deadline.expired() && deadlines.terminal.can_start_route()
        {
            SearchStop::always()
        } else {
            deadline.hard_stop()
        };
    let split = refine_with_terminal_source_split_floor_until(
        parent,
        context.options,
        context.source.decoded_limit,
        context.source.identity,
        &mut split_stop,
    )?;
    split_step.finish(context.report_optional(split.as_ref()));
    if let Some(split) = split {
        candidate.replace_if_smaller(split);
    }
    Ok(())
}

/// Apply the terminal tree floors and source-certified restorations.
fn finish_terminal_searches(
    context: &RouteContext<'_>,
    deadlines: &StreamDeadlines,
    mut candidate: Candidate,
) -> Result<Candidate> {
    let RouteContext {
        source,
        options,
        default_floor,
        progress,
        ..
    } = *context;
    let terminal_work = DefaultFloorWork::Timed(deadlines.terminal_work());

    // Default runs these floors inside `improve_default_floor_with_feedback`
    // so Max can retain the exact same completed comparison endpoint. Max
    // applies them again only to its final incumbent, where they remain
    // additive and cannot discard that endpoint.
    if options.exhaustive {
        candidate =
            improve_with_terminal_tree_floors(source, options, terminal_work, progress, candidate)?;
    }

    // Restore source-certified choices only after the established lineages
    // finish. The same terminal pass is included in Max's mandatory Default
    // endpoint, without changing the historical seed used by its searches.
    let restoration_work = if default_floor == DefaultFloor::MandatoryComplete {
        DefaultFloorWork::Mandatory
    } else {
        terminal_work
    };
    improve_with_terminal_searches(
        source,
        options,
        restoration_work,
        terminal_work,
        progress,
        candidate,
    )
}

#[cfg(test)]
mod tests;
