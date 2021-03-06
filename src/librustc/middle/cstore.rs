// Copyright 2015 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

// Copyright 2015 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

// the rustc crate store interface. This also includes types that
// are *mostly* used as a part of that interface, but these should
// probably get a better home if someone can find one.

use hir::def;
use hir::def_id::{CrateNum, DefId, DefIndex};
use hir::map as hir_map;
use hir::map::definitions::{Definitions, DefKey, DefPathTable};
use hir::svh::Svh;
use ich;
use middle::lang_items;
use ty::{self, TyCtxt};
use session::Session;
use session::search_paths::PathKind;
use util::nodemap::{NodeSet, DefIdMap};

use std::any::Any;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use owning_ref::ErasedBoxRef;
use syntax::ast;
use syntax::ext::base::SyntaxExtension;
use syntax::symbol::Symbol;
use syntax_pos::Span;
use rustc_back::target::Target;
use hir;
use rustc_back::PanicStrategy;

pub use self::NativeLibraryKind::*;

// lonely orphan structs and enums looking for a better home

#[derive(Clone, Debug)]
pub struct LinkMeta {
    pub crate_hash: Svh,
}

// Where a crate came from on the local filesystem. One of these three options
// must be non-None.
#[derive(PartialEq, Clone, Debug)]
pub struct CrateSource {
    pub dylib: Option<(PathBuf, PathKind)>,
    pub rlib: Option<(PathBuf, PathKind)>,
    pub rmeta: Option<(PathBuf, PathKind)>,
}

#[derive(RustcEncodable, RustcDecodable, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Debug)]
pub enum DepKind {
    /// A dependency that is only used for its macros, none of which are visible from other crates.
    /// These are included in the metadata only as placeholders and are ignored when decoding.
    UnexportedMacrosOnly,
    /// A dependency that is only used for its macros.
    MacrosOnly,
    /// A dependency that is always injected into the dependency list and so
    /// doesn't need to be linked to an rlib, e.g. the injected allocator.
    Implicit,
    /// A dependency that is required by an rlib version of this crate.
    /// Ordinary `extern crate`s result in `Explicit` dependencies.
    Explicit,
}

impl DepKind {
    pub fn macros_only(self) -> bool {
        match self {
            DepKind::UnexportedMacrosOnly | DepKind::MacrosOnly => true,
            DepKind::Implicit | DepKind::Explicit => false,
        }
    }
}

#[derive(PartialEq, Clone, Debug)]
pub enum LibSource {
    Some(PathBuf),
    MetadataOnly,
    None,
}

impl LibSource {
    pub fn is_some(&self) -> bool {
        if let LibSource::Some(_) = *self {
            true
        } else {
            false
        }
    }

    pub fn option(&self) -> Option<PathBuf> {
        match *self {
            LibSource::Some(ref p) => Some(p.clone()),
            LibSource::MetadataOnly | LibSource::None => None,
        }
    }
}

#[derive(Copy, Debug, PartialEq, Clone, RustcEncodable, RustcDecodable)]
pub enum LinkagePreference {
    RequireDynamic,
    RequireStatic,
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, RustcEncodable, RustcDecodable)]
pub enum NativeLibraryKind {
    NativeStatic,    // native static library (.a archive)
    NativeStaticNobundle, // native static library, which doesn't get bundled into .rlibs
    NativeFramework, // macOS-specific
    NativeUnknown,   // default way to specify a dynamic library
}

#[derive(Clone, Hash, RustcEncodable, RustcDecodable)]
pub struct NativeLibrary {
    pub kind: NativeLibraryKind,
    pub name: Symbol,
    pub cfg: Option<ast::MetaItem>,
    pub foreign_items: Vec<DefIndex>,
}

pub enum LoadedMacro {
    MacroDef(ast::Item),
    ProcMacro(Rc<SyntaxExtension>),
}

#[derive(Copy, Clone, Debug)]
pub struct ExternCrate {
    /// def_id of an `extern crate` in the current crate that caused
    /// this crate to be loaded; note that there could be multiple
    /// such ids
    pub def_id: DefId,

    /// span of the extern crate that caused this to be loaded
    pub span: Span,

    /// If true, then this crate is the crate named by the extern
    /// crate referenced above. If false, then this crate is a dep
    /// of the crate.
    pub direct: bool,

    /// Number of links to reach the extern crate `def_id`
    /// declaration; used to select the extern crate with the shortest
    /// path
    pub path_len: usize,
}

pub struct EncodedMetadata {
    pub raw_data: Vec<u8>,
    pub hashes: EncodedMetadataHashes,
}

impl EncodedMetadata {
    pub fn new() -> EncodedMetadata {
        EncodedMetadata {
            raw_data: Vec::new(),
            hashes: EncodedMetadataHashes::new(),
        }
    }
}

/// The hash for some metadata that (when saving) will be exported
/// from this crate, or which (when importing) was exported by an
/// upstream crate.
#[derive(Debug, RustcEncodable, RustcDecodable, Copy, Clone)]
pub struct EncodedMetadataHash {
    pub def_index: DefIndex,
    pub hash: ich::Fingerprint,
}

/// The hash for some metadata that (when saving) will be exported
/// from this crate, or which (when importing) was exported by an
/// upstream crate.
#[derive(Debug, RustcEncodable, RustcDecodable, Clone)]
pub struct EncodedMetadataHashes {
    // Stable content hashes for things in crate metadata, indexed by DefIndex.
    pub hashes: Vec<EncodedMetadataHash>,
}

impl EncodedMetadataHashes {
    pub fn new() -> EncodedMetadataHashes {
        EncodedMetadataHashes {
            hashes: Vec::new(),
        }
    }
}

/// The backend's way to give the crate store access to the metadata in a library.
/// Note that it returns the raw metadata bytes stored in the library file, whether
/// it is compressed, uncompressed, some weird mix, etc.
/// rmeta files are backend independent and not handled here.
///
/// At the time of this writing, there is only one backend and one way to store
/// metadata in library -- this trait just serves to decouple rustc_metadata from
/// the archive reader, which depends on LLVM.
pub trait MetadataLoader {
    fn get_rlib_metadata(&self,
                         target: &Target,
                         filename: &Path)
                         -> Result<ErasedBoxRef<[u8]>, String>;
    fn get_dylib_metadata(&self,
                          target: &Target,
                          filename: &Path)
                          -> Result<ErasedBoxRef<[u8]>, String>;
}

/// A store of Rust crates, through with their metadata
/// can be accessed.
pub trait CrateStore {
    fn crate_data_as_rc_any(&self, krate: CrateNum) -> Rc<Any>;

    // access to the metadata loader
    fn metadata_loader(&self) -> &MetadataLoader;

    // item info
    fn visibility(&self, def: DefId) -> ty::Visibility;
    fn visible_parent_map<'a>(&'a self, sess: &Session) -> ::std::cell::Ref<'a, DefIdMap<DefId>>;
    fn item_generics_cloned(&self, def: DefId) -> ty::Generics;

    // trait info
    fn implementations_of_trait(&self, filter: Option<DefId>) -> Vec<DefId>;

    // impl info
    fn impl_defaultness(&self, def: DefId) -> hir::Defaultness;

    // trait/impl-item info
    fn associated_item_cloned(&self, def: DefId) -> ty::AssociatedItem;

    // flags
    fn is_dllimport_foreign_item(&self, def: DefId) -> bool;
    fn is_statically_included_foreign_item(&self, def_id: DefId) -> bool;

    // crate metadata
    fn dep_kind(&self, cnum: CrateNum) -> DepKind;
    fn export_macros(&self, cnum: CrateNum);
    fn lang_items(&self, cnum: CrateNum) -> Vec<(DefIndex, usize)>;
    fn missing_lang_items(&self, cnum: CrateNum) -> Vec<lang_items::LangItem>;
    fn is_compiler_builtins(&self, cnum: CrateNum) -> bool;
    fn is_sanitizer_runtime(&self, cnum: CrateNum) -> bool;
    fn is_profiler_runtime(&self, cnum: CrateNum) -> bool;
    fn panic_strategy(&self, cnum: CrateNum) -> PanicStrategy;
    /// The name of the crate as it is referred to in source code of the current
    /// crate.
    fn crate_name(&self, cnum: CrateNum) -> Symbol;
    /// The name of the crate as it is stored in the crate's metadata.
    fn original_crate_name(&self, cnum: CrateNum) -> Symbol;
    fn crate_hash(&self, cnum: CrateNum) -> Svh;
    fn crate_disambiguator(&self, cnum: CrateNum) -> Symbol;
    fn plugin_registrar_fn(&self, cnum: CrateNum) -> Option<DefId>;
    fn derive_registrar_fn(&self, cnum: CrateNum) -> Option<DefId>;
    fn native_libraries(&self, cnum: CrateNum) -> Vec<NativeLibrary>;
    fn exported_symbols(&self, cnum: CrateNum) -> Vec<DefId>;
    fn is_no_builtins(&self, cnum: CrateNum) -> bool;

    // resolve
    fn def_key(&self, def: DefId) -> DefKey;
    fn def_path(&self, def: DefId) -> hir_map::DefPath;
    fn def_path_hash(&self, def: DefId) -> hir_map::DefPathHash;
    fn def_path_table(&self, cnum: CrateNum) -> Rc<DefPathTable>;
    fn struct_field_names(&self, def: DefId) -> Vec<ast::Name>;
    fn item_children(&self, did: DefId, sess: &Session) -> Vec<def::Export>;
    fn load_macro(&self, did: DefId, sess: &Session) -> LoadedMacro;

    // misc. metadata
    fn item_body<'a, 'tcx>(&self, tcx: TyCtxt<'a, 'tcx, 'tcx>, def: DefId)
                           -> &'tcx hir::Body;

    // This is basically a 1-based range of ints, which is a little
    // silly - I may fix that.
    fn crates(&self) -> Vec<CrateNum>;
    fn used_libraries(&self) -> Vec<NativeLibrary>;
    fn used_link_args(&self) -> Vec<String>;

    // utility functions
    fn used_crates(&self, prefer: LinkagePreference) -> Vec<(CrateNum, LibSource)>;
    fn used_crate_source(&self, cnum: CrateNum) -> CrateSource;
    fn extern_mod_stmt_cnum(&self, emod_id: ast::NodeId) -> Option<CrateNum>;
    fn encode_metadata<'a, 'tcx>(&self,
                                 tcx: TyCtxt<'a, 'tcx, 'tcx>,
                                 link_meta: &LinkMeta,
                                 reachable: &NodeSet)
                                 -> EncodedMetadata;
    fn metadata_encoding_version(&self) -> &[u8];
}

// FIXME: find a better place for this?
pub fn validate_crate_name(sess: Option<&Session>, s: &str, sp: Option<Span>) {
    let mut err_count = 0;
    {
        let mut say = |s: &str| {
            match (sp, sess) {
                (_, None) => bug!("{}", s),
                (Some(sp), Some(sess)) => sess.span_err(sp, s),
                (None, Some(sess)) => sess.err(s),
            }
            err_count += 1;
        };
        if s.is_empty() {
            say("crate name must not be empty");
        }
        for c in s.chars() {
            if c.is_alphanumeric() { continue }
            if c == '_'  { continue }
            say(&format!("invalid character `{}` in crate name: `{}`", c, s));
        }
    }

    if err_count > 0 {
        sess.unwrap().abort_if_errors();
    }
}

/// A dummy crate store that does not support any non-local crates,
/// for test purposes.
pub struct DummyCrateStore;

#[allow(unused_variables)]
impl CrateStore for DummyCrateStore {
    fn crate_data_as_rc_any(&self, krate: CrateNum) -> Rc<Any>
        { bug!("crate_data_as_rc_any") }
    // item info
    fn visibility(&self, def: DefId) -> ty::Visibility { bug!("visibility") }
    fn visible_parent_map<'a>(&'a self, session: &Session)
        -> ::std::cell::Ref<'a, DefIdMap<DefId>>
    {
        bug!("visible_parent_map")
    }
    fn item_generics_cloned(&self, def: DefId) -> ty::Generics
        { bug!("item_generics_cloned") }

    // trait info
    fn implementations_of_trait(&self, filter: Option<DefId>) -> Vec<DefId> { vec![] }

    // impl info
    fn impl_defaultness(&self, def: DefId) -> hir::Defaultness { bug!("impl_defaultness") }

    // trait/impl-item info
    fn associated_item_cloned(&self, def: DefId) -> ty::AssociatedItem
        { bug!("associated_item_cloned") }

    // flags
    fn is_dllimport_foreign_item(&self, id: DefId) -> bool { false }
    fn is_statically_included_foreign_item(&self, def_id: DefId) -> bool { false }

    // crate metadata
    fn lang_items(&self, cnum: CrateNum) -> Vec<(DefIndex, usize)>
        { bug!("lang_items") }
    fn missing_lang_items(&self, cnum: CrateNum) -> Vec<lang_items::LangItem>
        { bug!("missing_lang_items") }
    fn dep_kind(&self, cnum: CrateNum) -> DepKind { bug!("is_explicitly_linked") }
    fn export_macros(&self, cnum: CrateNum) { bug!("export_macros") }
    fn is_compiler_builtins(&self, cnum: CrateNum) -> bool { bug!("is_compiler_builtins") }
    fn is_profiler_runtime(&self, cnum: CrateNum) -> bool { bug!("is_profiler_runtime") }
    fn is_sanitizer_runtime(&self, cnum: CrateNum) -> bool { bug!("is_sanitizer_runtime") }
    fn panic_strategy(&self, cnum: CrateNum) -> PanicStrategy {
        bug!("panic_strategy")
    }
    fn crate_name(&self, cnum: CrateNum) -> Symbol { bug!("crate_name") }
    fn original_crate_name(&self, cnum: CrateNum) -> Symbol {
        bug!("original_crate_name")
    }
    fn crate_hash(&self, cnum: CrateNum) -> Svh { bug!("crate_hash") }
    fn crate_disambiguator(&self, cnum: CrateNum)
                           -> Symbol { bug!("crate_disambiguator") }
    fn plugin_registrar_fn(&self, cnum: CrateNum) -> Option<DefId>
        { bug!("plugin_registrar_fn") }
    fn derive_registrar_fn(&self, cnum: CrateNum) -> Option<DefId>
        { bug!("derive_registrar_fn") }
    fn native_libraries(&self, cnum: CrateNum) -> Vec<NativeLibrary>
        { bug!("native_libraries") }
    fn exported_symbols(&self, cnum: CrateNum) -> Vec<DefId> { bug!("exported_symbols") }
    fn is_no_builtins(&self, cnum: CrateNum) -> bool { bug!("is_no_builtins") }

    // resolve
    fn def_key(&self, def: DefId) -> DefKey { bug!("def_key") }
    fn def_path(&self, def: DefId) -> hir_map::DefPath {
        bug!("relative_def_path")
    }
    fn def_path_hash(&self, def: DefId) -> hir_map::DefPathHash {
        bug!("def_path_hash")
    }
    fn def_path_table(&self, cnum: CrateNum) -> Rc<DefPathTable> {
        bug!("def_path_table")
    }
    fn struct_field_names(&self, def: DefId) -> Vec<ast::Name> { bug!("struct_field_names") }
    fn item_children(&self, did: DefId, sess: &Session) -> Vec<def::Export> {
        bug!("item_children")
    }
    fn load_macro(&self, did: DefId, sess: &Session) -> LoadedMacro { bug!("load_macro") }

    // misc. metadata
    fn item_body<'a, 'tcx>(&self, tcx: TyCtxt<'a, 'tcx, 'tcx>, def: DefId)
                           -> &'tcx hir::Body {
        bug!("item_body")
    }

    // This is basically a 1-based range of ints, which is a little
    // silly - I may fix that.
    fn crates(&self) -> Vec<CrateNum> { vec![] }
    fn used_libraries(&self) -> Vec<NativeLibrary> { vec![] }
    fn used_link_args(&self) -> Vec<String> { vec![] }

    // utility functions
    fn used_crates(&self, prefer: LinkagePreference) -> Vec<(CrateNum, LibSource)>
        { vec![] }
    fn used_crate_source(&self, cnum: CrateNum) -> CrateSource { bug!("used_crate_source") }
    fn extern_mod_stmt_cnum(&self, emod_id: ast::NodeId) -> Option<CrateNum> { None }
    fn encode_metadata<'a, 'tcx>(&self,
                                 tcx: TyCtxt<'a, 'tcx, 'tcx>,
                                 link_meta: &LinkMeta,
                                 reachable: &NodeSet)
                                 -> EncodedMetadata {
        bug!("encode_metadata")
    }
    fn metadata_encoding_version(&self) -> &[u8] { bug!("metadata_encoding_version") }

    // access to the metadata loader
    fn metadata_loader(&self) -> &MetadataLoader { bug!("metadata_loader") }
}

pub trait CrateLoader {
    fn process_item(&mut self, item: &ast::Item, defs: &Definitions);
    fn postprocess(&mut self, krate: &ast::Crate);
}
