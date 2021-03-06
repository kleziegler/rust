// Copyright 2016 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

// The visitors in this module collect sizes and counts of the most important
// pieces of MIR. The resulting numbers are good approximations but not
// completely accurate (some things might be counted twice, others missed).

use rustc_const_math::{ConstUsize};
use rustc::hir::def_id::LOCAL_CRATE;
use rustc::middle::const_val::{ConstVal};
use rustc::mir::{AggregateKind, AssertMessage, BasicBlock, BasicBlockData};
use rustc::mir::{Constant, Literal, Location, LocalDecl};
use rustc::mir::{Lvalue, LvalueElem, LvalueProjection};
use rustc::mir::{Mir, Operand, ProjectionElem};
use rustc::mir::{Rvalue, SourceInfo, Statement, StatementKind};
use rustc::mir::{Terminator, TerminatorKind, VisibilityScope, VisibilityScopeData};
use rustc::mir::visit as mir_visit;
use rustc::mir::visit::Visitor;
use rustc::ty::{ClosureSubsts, TyCtxt};
use rustc::util::common::to_readable_str;
use rustc::util::nodemap::{FxHashMap};

struct NodeData {
    count: usize,
    size: usize,
}

struct StatCollector<'a, 'tcx: 'a> {
    _tcx: TyCtxt<'a, 'tcx, 'tcx>,
    data: FxHashMap<&'static str, NodeData>,
}

pub fn print_mir_stats<'tcx, 'a>(tcx: TyCtxt<'a, 'tcx, 'tcx>, title: &str) {
    let mut collector = StatCollector {
        _tcx: tcx,
        data: FxHashMap(),
    };
    // For debugging instrumentation like this, we don't need to worry
    // about maintaining the dep graph.
    let _ignore = tcx.dep_graph.in_ignore();
    for &def_id in tcx.mir_keys(LOCAL_CRATE).iter() {
        let mir = tcx.optimized_mir(def_id);
        collector.visit_mir(&mir);
    }
    collector.print(title);
}

impl<'a, 'tcx> StatCollector<'a, 'tcx> {

    fn record_with_size(&mut self, label: &'static str, node_size: usize) {
        let entry = self.data.entry(label).or_insert(NodeData {
            count: 0,
            size: 0,
        });

        entry.count += 1;
        entry.size = node_size;
    }

    fn record<T>(&mut self, label: &'static str, node: &T) {
        self.record_with_size(label, ::std::mem::size_of_val(node));
    }

    fn print(&self, title: &str) {
        let mut stats: Vec<_> = self.data.iter().collect();

        stats.sort_by_key(|&(_, ref d)| d.count * d.size);

        println!("\n{}\n", title);

        println!("{:<32}{:>18}{:>14}{:>14}",
            "Name", "Accumulated Size", "Count", "Item Size");
        println!("------------------------------------------------------------------------------");

        for (label, data) in stats {
            println!("{:<32}{:>18}{:>14}{:>14}",
                label,
                to_readable_str(data.count * data.size),
                to_readable_str(data.count),
                to_readable_str(data.size));
        }
        println!("------------------------------------------------------------------------------");
    }
}

impl<'a, 'tcx> mir_visit::Visitor<'tcx> for StatCollector<'a, 'tcx> {
    fn visit_mir(&mut self, mir: &Mir<'tcx>) {
        self.record("Mir", mir);

        // since the `super_mir` method does not traverse the MIR of
        // promoted rvalues, (but we still want to gather statistics
        // on the structures represented there) we manually traverse
        // the promoted rvalues here.
        for promoted_mir in &mir.promoted {
            self.visit_mir(promoted_mir);
        }

        self.super_mir(mir);
    }

    fn visit_basic_block_data(&mut self,
                              block: BasicBlock,
                              data: &BasicBlockData<'tcx>) {
        self.record("BasicBlockData", data);
        self.super_basic_block_data(block, data);
    }

    fn visit_visibility_scope_data(&mut self,
                                   scope_data: &VisibilityScopeData) {
        self.record("VisibilityScopeData", scope_data);
        self.super_visibility_scope_data(scope_data);
    }

    fn visit_statement(&mut self,
                       block: BasicBlock,
                       statement: &Statement<'tcx>,
                       location: Location) {
        self.record("Statement", statement);
        self.record(match statement.kind {
            StatementKind::Assign(..) => "StatementKind::Assign",
            StatementKind::EndRegion(..) => "StatementKind::EndRegion",
            StatementKind::SetDiscriminant { .. } => "StatementKind::SetDiscriminant",
            StatementKind::StorageLive(..) => "StatementKind::StorageLive",
            StatementKind::StorageDead(..) => "StatementKind::StorageDead",
            StatementKind::InlineAsm { .. } => "StatementKind::InlineAsm",
            StatementKind::Nop => "StatementKind::Nop",
        }, &statement.kind);
        self.super_statement(block, statement, location);
    }

    fn visit_terminator(&mut self,
                        block: BasicBlock,
                        terminator: &Terminator<'tcx>,
                        location: Location) {
        self.record("Terminator", terminator);
        self.super_terminator(block, terminator, location);
    }

    fn visit_terminator_kind(&mut self,
                             block: BasicBlock,
                             kind: &TerminatorKind<'tcx>,
                             location: Location) {
        self.record("TerminatorKind", kind);
        self.record(match *kind {
            TerminatorKind::Goto { .. } => "TerminatorKind::Goto",
            TerminatorKind::SwitchInt { .. } => "TerminatorKind::SwitchInt",
            TerminatorKind::Resume => "TerminatorKind::Resume",
            TerminatorKind::Return => "TerminatorKind::Return",
            TerminatorKind::Unreachable => "TerminatorKind::Unreachable",
            TerminatorKind::Drop { .. } => "TerminatorKind::Drop",
            TerminatorKind::DropAndReplace { .. } => "TerminatorKind::DropAndReplace",
            TerminatorKind::Call { .. } => "TerminatorKind::Call",
            TerminatorKind::Assert { .. } => "TerminatorKind::Assert",
        }, kind);
        self.super_terminator_kind(block, kind, location);
    }

    fn visit_assert_message(&mut self,
                            msg: &AssertMessage<'tcx>,
                            location: Location) {
        self.record("AssertMessage", msg);
        self.record(match *msg {
            AssertMessage::BoundsCheck { .. } => "AssertMessage::BoundsCheck",
            AssertMessage::Math(..) => "AssertMessage::Math",
        }, msg);
        self.super_assert_message(msg, location);
    }

    fn visit_rvalue(&mut self,
                    rvalue: &Rvalue<'tcx>,
                    location: Location) {
        self.record("Rvalue", rvalue);
        let rvalue_kind = match *rvalue {
            Rvalue::Use(..) => "Rvalue::Use",
            Rvalue::Repeat(..) => "Rvalue::Repeat",
            Rvalue::Ref(..) => "Rvalue::Ref",
            Rvalue::Len(..) => "Rvalue::Len",
            Rvalue::Cast(..) => "Rvalue::Cast",
            Rvalue::BinaryOp(..) => "Rvalue::BinaryOp",
            Rvalue::CheckedBinaryOp(..) => "Rvalue::CheckedBinaryOp",
            Rvalue::UnaryOp(..) => "Rvalue::UnaryOp",
            Rvalue::Discriminant(..) => "Rvalue::Discriminant",
            Rvalue::NullaryOp(..) => "Rvalue::NullaryOp",
            Rvalue::Aggregate(ref kind, ref _operands) => {
                // AggregateKind is not distinguished by visit API, so
                // record it. (`super_rvalue` handles `_operands`.)
                self.record(match **kind {
                    AggregateKind::Array(_) => "AggregateKind::Array",
                    AggregateKind::Tuple => "AggregateKind::Tuple",
                    AggregateKind::Adt(..) => "AggregateKind::Adt",
                    AggregateKind::Closure(..) => "AggregateKind::Closure",
                }, kind);

                "Rvalue::Aggregate"
            }
        };
        self.record(rvalue_kind, rvalue);
        self.super_rvalue(rvalue, location);
    }

    fn visit_operand(&mut self,
                     operand: &Operand<'tcx>,
                     location: Location) {
        self.record("Operand", operand);
        self.record(match *operand {
            Operand::Consume(..) => "Operand::Consume",
            Operand::Constant(..) => "Operand::Constant",
        }, operand);
        self.super_operand(operand, location);
    }

    fn visit_lvalue(&mut self,
                    lvalue: &Lvalue<'tcx>,
                    context: mir_visit::LvalueContext<'tcx>,
                    location: Location) {
        self.record("Lvalue", lvalue);
        self.record(match *lvalue {
            Lvalue::Local(..) => "Lvalue::Local",
            Lvalue::Static(..) => "Lvalue::Static",
            Lvalue::Projection(..) => "Lvalue::Projection",
        }, lvalue);
        self.super_lvalue(lvalue, context, location);
    }

    fn visit_projection(&mut self,
                        lvalue: &LvalueProjection<'tcx>,
                        context: mir_visit::LvalueContext<'tcx>,
                        location: Location) {
        self.record("LvalueProjection", lvalue);
        self.super_projection(lvalue, context, location);
    }

    fn visit_projection_elem(&mut self,
                             lvalue: &LvalueElem<'tcx>,
                             context: mir_visit::LvalueContext<'tcx>,
                             location: Location) {
        self.record("LvalueElem", lvalue);
        self.record(match *lvalue {
            ProjectionElem::Deref => "LvalueElem::Deref",
            ProjectionElem::Subslice { .. } => "LvalueElem::Subslice",
            ProjectionElem::Field(..) => "LvalueElem::Field",
            ProjectionElem::Index(..) => "LvalueElem::Index",
            ProjectionElem::ConstantIndex { .. } => "LvalueElem::ConstantIndex",
            ProjectionElem::Downcast(..) => "LvalueElem::Downcast",
        }, lvalue);
        self.super_projection_elem(lvalue, context, location);
    }

    fn visit_constant(&mut self,
                      constant: &Constant<'tcx>,
                      location: Location) {
        self.record("Constant", constant);
        self.super_constant(constant, location);
    }

    fn visit_literal(&mut self,
                     literal: &Literal<'tcx>,
                     location: Location) {
        self.record("Literal", literal);
        self.record(match *literal {
            Literal::Item { .. } => "Literal::Item",
            Literal::Value { .. } => "Literal::Value",
            Literal::Promoted { .. } => "Literal::Promoted",
        }, literal);
        self.super_literal(literal, location);
    }

    fn visit_source_info(&mut self,
                         source_info: &SourceInfo) {
        self.record("SourceInfo", source_info);
        self.super_source_info(source_info);
    }

    fn visit_closure_substs(&mut self,
                            substs: &ClosureSubsts<'tcx>,
                            _: Location) {
        self.record("ClosureSubsts", substs);
        self.super_closure_substs(substs);
    }

    fn visit_const_val(&mut self,
                       const_val: &ConstVal,
                       _: Location) {
        self.record("ConstVal", const_val);
        self.super_const_val(const_val);
    }

    fn visit_const_usize(&mut self,
                         const_usize: &ConstUsize,
                         _: Location) {
        self.record("ConstUsize", const_usize);
        self.super_const_usize(const_usize);
    }

    fn visit_local_decl(&mut self,
                        local_decl: &LocalDecl<'tcx>) {
        self.record("LocalDecl", local_decl);
        self.super_local_decl(local_decl);
    }

    fn visit_visibility_scope(&mut self,
                              scope: &VisibilityScope) {
        self.record("VisiblityScope", scope);
        self.super_visibility_scope(scope);
    }
}
