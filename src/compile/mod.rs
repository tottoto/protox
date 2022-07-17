use std::{
    collections::HashMap,
    fmt::{self, Write},
    ops::{Index, IndexMut},
    path::{Path, PathBuf},
    sync::Arc,
};

use logos::Span;
use miette::NamedSource;
use prost::Message;

use crate::{
    ast,
    check::{check_with_names, NameMap},
    error::{DynSourceCode, Error, ErrorKind},
    file::{
        check_shadow, path_to_file_name, ChainFileResolver, FileResolver, GoogleFileResolver,
        IncludeFileResolver,
    },
    parse, transcode_file,
    types::{FileDescriptorProto, FileDescriptorSet},
    MAX_FILE_LEN,
};

#[cfg(test)]
mod tests;

/// Options for compiling protobuf files.
pub struct Compiler {
    resolver: Box<dyn FileResolver>,
    file_map: ParsedFileMap,
    include_imports: bool,
    include_source_info: bool,
}

#[derive(Debug)]
pub(crate) struct ParsedFile {
    pub descriptor: FileDescriptorProto,
    pub name_map: NameMap,
    pub path: Option<PathBuf>,
    pub is_root: bool,
}

#[derive(Debug, Default)]
pub(crate) struct ParsedFileMap {
    files: Vec<ParsedFile>,
    file_names: HashMap<String, usize>,
}

impl Compiler {
    /// Create a new [`Compiler`] with default options and the given set of include paths.
    ///
    /// In addition to the given include paths, the [`Compiler`] instance will be able to import
    /// standard files like `google/protobuf/descriptor.proto`.
    pub fn new(includes: impl IntoIterator<Item = impl AsRef<Path>>) -> Result<Self, Error> {
        let mut resolver = ChainFileResolver::new();

        for include in includes {
            resolver.add(IncludeFileResolver::new(include.as_ref().to_owned()));
        }

        resolver.add(GoogleFileResolver::new());

        Ok(Compiler::with_file_resolver(resolver))
    }

    /// Create a new [`Compiler`] with a custom [`FileResolver`] for looking up imported files.
    pub fn with_file_resolver<R>(resolver: R) -> Self
    where
        R: FileResolver + 'static,
    {
        Compiler {
            resolver: Box::new(resolver),
            file_map: Default::default(),
            include_imports: false,
            include_source_info: false,
        }
    }

    /// Set whether the output `FileDescriptorSet` should include source info.
    ///
    /// If set, the file descriptors returned by [`file_descriptor_set`](Compiler::file_descriptor_set) will have
    /// the [`FileDescriptorProto::source_code_info`](prost_types::FileDescriptorProto::source_code_info) field
    /// populated with source locations and comments.
    pub fn include_source_info(&mut self, yes: bool) -> &mut Self {
        self.include_source_info = yes;
        self
    }

    /// Set whether the output `FileDescriptorSet` should include imported files.
    ///
    /// By default, only files explicitly added with [`add_file`](Compiler::add_file) are returned by [`file_descriptor_set`](Compiler::file_descriptor_set).
    /// If this option is set, imported files are included too.
    pub fn include_imports(&mut self, yes: bool) -> &mut Self {
        self.include_imports = yes;
        self
    }

    /// Compile the file at the given path, and add it to this `Compiler` instance.
    ///
    /// If the path is absolute, or relative to the current directory, it must reside under one of the
    /// include paths. Otherwise, it is looked up relative to the given include paths in the same way as
    /// `import` statements.
    pub fn add_file(&mut self, relative_path: impl AsRef<Path>) -> Result<&mut Self, Error> {
        let relative_path = relative_path.as_ref();
        let name = match self
            .resolver
            .resolve_path(relative_path)
            .or_else(|| path_to_file_name(relative_path))
        {
            Some(name) => name,
            None => {
                return Err(Error::from_kind(ErrorKind::FileNotIncluded {
                    path: relative_path.to_owned(),
                }))
            }
        };

        if let Some(parsed_file) = self.file_map.get_mut(&name) {
            check_shadow(&parsed_file.path, relative_path)?;
            parsed_file.is_root = true;
            return Ok(self);
        }

        let file = self.resolver.open_file(&name).map_err(|err| {
            if err.is_file_not_found() {
                Error::from_kind(ErrorKind::FileNotIncluded {
                    path: relative_path.to_owned(),
                })
            } else {
                err
            }
        })?;
        check_shadow(&file.path, relative_path)?;

        if file.content.len() > (MAX_FILE_LEN as usize) {
            return Err(Error::from_kind(ErrorKind::FileTooLarge {
                src: DynSourceCode::default(),
                span: None,
            }));
        }

        let source: Arc<str> = file.content.into();
        let ast = match parse::parse(&source) {
            Ok(ast) => ast,
            Err(errors) => {
                return Err(Error::parse_errors(
                    errors,
                    make_source(&name, &file.path, source),
                ));
            }
        };

        let mut import_stack = vec![name.clone()];
        for import in &ast.imports {
            self.add_import(
                &import.value,
                Some(import.span.clone()),
                &mut import_stack,
                make_source(&name, &file.path, source.clone()),
            )?;
        }

        let (descriptor, name_map) = self.check_file(&name, &ast, source, &file.path)?;

        self.file_map.add(ParsedFile {
            descriptor,
            name_map,
            path: file.path,
            is_root: true,
        });
        Ok(self)
    }

    // TODO:
    // - should the added descriptor be returned with include_imports?
    // - how do we handle resolution of relative type names etc?

    #[doc(hidden)]
    pub fn add_file_descriptor_proto(
        &mut self,
        descriptor: prost_types::FileDescriptorProto,
    ) -> Result<&mut Self, Error> {
        if self.file_map.file_names.contains_key(descriptor.name()) {
            return Ok(self);
        }

        let descriptor: FileDescriptorProto = transcode_file(&descriptor, &mut Vec::new());

        let mut import_stack = vec![descriptor.name().to_owned()];
        for import in &descriptor.dependency {
            self.add_import(import, None, &mut import_stack, DynSourceCode::default())?;
        }

        let name_map = NameMap::from_proto(&descriptor, &self.file_map)
            .map_err(|errors| Error::check_errors(errors, DynSourceCode::default()))?;

        self.file_map.add(ParsedFile {
            descriptor,
            name_map,
            path: None,
            is_root: true, // TODO should this be configurable?
        });
        Ok(self)
    }

    /// Convert all added files into an instance of [`FileDescriptorSet`](prost_types::FileDescriptorSet).
    ///
    /// Files are sorted topologically, with dependency files ordered before the files that import them.
    pub fn file_descriptor_set(&self) -> prost_types::FileDescriptorSet {
        let mut buf = Vec::new();

        let file = if self.include_imports {
            self.file_map
                .files
                .iter()
                .map(|f| transcode_file(&f.descriptor, &mut buf))
                .collect()
        } else {
            self.file_map
                .files
                .iter()
                .filter(|f| f.is_root)
                .map(|f| transcode_file(&f.descriptor, &mut buf))
                .collect()
        };

        prost_types::FileDescriptorSet { file }
    }

    /// Convert all added files into an instance of [`FileDescriptorSet`](prost_types::FileDescriptorSet) and encodes it.
    ///
    /// This is equivalent to `file_descriptor_set()?.encode_to_vec()`, with the exception that extension
    /// options are included.
    pub fn encode_file_descriptor_set(&self) -> Vec<u8> {
        let file = if self.include_imports {
            self.file_map
                .files
                .iter()
                .map(|f| f.descriptor.clone())
                .collect()
        } else {
            self.file_map
                .files
                .iter()
                .filter(|f| f.is_root)
                .map(|f| f.descriptor.clone())
                .collect()
        };

        FileDescriptorSet { file }.encode_to_vec()
    }

    pub(crate) fn into_parsed_file_map(self) -> ParsedFileMap {
        self.file_map
    }

    fn add_import(
        &mut self,
        file_name: &str,
        span: Option<Span>,
        import_stack: &mut Vec<String>,
        import_src: DynSourceCode,
    ) -> Result<(), Error> {
        if import_stack.iter().any(|name| name == file_name) {
            let mut cycle = String::new();
            for import in import_stack {
                write!(&mut cycle, "{} -> ", import).unwrap();
            }
            write!(&mut cycle, "{}", file_name).unwrap();

            return Err(Error::from_kind(ErrorKind::CircularImport { cycle }));
        }

        if self.file_map.file_names.contains_key(file_name) {
            return Ok(());
        }

        let file = match self.resolver.open_file(file_name) {
            Ok(file) if file.content.len() > (MAX_FILE_LEN as usize) => {
                return Err(Error::from_kind(ErrorKind::FileTooLarge {
                    src: import_src,
                    span: span.map(Into::into),
                }));
            }
            Ok(file) => file,
            Err(err) => return Err(err.add_import_context(import_src, span)),
        };

        let source: Arc<str> = file.content.into();
        let ast = match parse::parse(&source) {
            Ok(ast) => ast,
            Err(errors) => {
                return Err(Error::parse_errors(
                    errors,
                    make_source(file_name, &file.path, source),
                ));
            }
        };

        import_stack.push(file_name.to_owned());
        for import in &ast.imports {
            self.add_import(
                &import.value,
                Some(import.span.clone()),
                import_stack,
                make_source(&import.value, &file.path, source.clone()),
            )?;
        }
        import_stack.pop();

        let (descriptor, name_map) = self.check_file(file_name, &ast, source, &file.path)?;

        self.file_map.add(ParsedFile {
            descriptor,
            name_map,
            path: file.path,
            is_root: false,
        });
        Ok(())
    }

    fn check_file(
        &self,
        name: &str,
        ast: &ast::File,
        source: Arc<str>,
        path: &Option<PathBuf>,
    ) -> Result<(FileDescriptorProto, NameMap), Error> {
        let source_info = if self.include_source_info {
            Some(source.as_ref())
        } else {
            None
        };

        check_with_names(ast, Some(name), source_info, &self.file_map)
            .map_err(|errors| Error::check_errors(errors, make_source(name, path, source)))
    }
}

impl fmt::Debug for Compiler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Compiler")
            .field("file_map", &self.file_map)
            .field("include_imports", &self.include_imports)
            .field("include_source_info", &self.include_source_info)
            .finish_non_exhaustive()
    }
}

impl ParsedFile {
    pub fn name(&self) -> &str {
        self.descriptor.name()
    }
}

impl ParsedFileMap {
    fn add(&mut self, file: ParsedFile) {
        self.file_names
            .insert(file.name().to_owned(), self.files.len());
        self.files.push(file);
    }

    fn get_mut(&mut self, name: &str) -> Option<&mut ParsedFile> {
        match self.file_names.get(name).copied() {
            Some(i) => Some(&mut self.files[i]),
            None => None,
        }
    }
}

impl Index<usize> for ParsedFileMap {
    type Output = ParsedFile;

    fn index(&self, index: usize) -> &Self::Output {
        &self.files[index]
    }
}

impl<'a> Index<&'a str> for ParsedFileMap {
    type Output = ParsedFile;

    fn index(&self, index: &'a str) -> &Self::Output {
        &self.files[self.file_names[index]]
    }
}

impl<'a> IndexMut<&'a str> for ParsedFileMap {
    fn index_mut(&mut self, index: &'a str) -> &mut Self::Output {
        &mut self.files[self.file_names[index]]
    }
}

fn make_source(name: &str, path: &Option<PathBuf>, source: Arc<str>) -> DynSourceCode {
    let name = match path {
        Some(path) => path.display().to_string(),
        None => name.to_owned(),
    };

    NamedSource::new(name, source).into()
}
