//! This crate exists only as a way of isolating the internal
//! functionality of [`fn-fixture`] in a way that allows self-testing.
//!
//! [`fn-fixture`]: https://docs.rs/fn-fixture/

use std::{
    env::var,
    fs::DirEntry,
    path::PathBuf,
    format_args as fmt,
};

use proc_macro2::{
    Ident,
    Literal,
    Span,
    TokenStream,
};
use smallvec::SmallVec;
use syn::{
    export::ToTokens,
    ExprLit,
    FnArg,
    ItemFn,
    Lit,
    parse2,
    parse_str,
    Pat,
    PatIdent,
    PatType,
    Signature,
    Type,
    Generics,
};

use quote::quote;

mod traits;

use self::traits::*;

const INPUT_TXT: &str = "input.txt";
const INPUT_RS: &str = "input.rs";
const INPUT_BIN: &str = "input.bin";

#[doc(hidden)]
pub fn make_snapshots(path_attr: &TokenStream, item: &TokenStream) -> Result<TokenStream, TokenStream> {
    let (
        name,
        Generics {
            lt_token: generic_lt,
            gt_token: generic_gt,
            params: generic_params,
            where_clause: generic_where,
        },
        (param_name, param_type),
    ) = pull_function_description(item.clone())?;

    let actual_file_name = {
        let mut base_name = name.to_string();
        base_name.push_str(".actual.txt");
        base_name
    };
    let expected_file_name = {
        let mut base_name = name.to_string();
        base_name.push_str(".txt");
        base_name
    };

    if expected_file_name == INPUT_TXT {
        return ().compile_error(fmt!("Cannot use that name, as it conflicts with {} detection", INPUT_TXT))
    }

    let base_name = name.to_token_stream();

    let path = {
        let mut path = PathBuf::new();
        path.push(var("CARGO_MANIFEST_DIR").compile_err("No manifest directory env")?);
        path.push(
            if let Lit::Str(string) =
            parse2::<ExprLit>(path_attr.clone())
                .compile_error(fmt!("Expected literal path in attribute, received: {}\n\n", path_attr))?
                .lit
            {
                string.value()
            } else {
                return ().compile_error(fmt!("Expected literal path: {}", path_attr))
            }
        );
        path
    };

    let tag: TokenStream = "#[test]".parse().compile_err("Failed to init tag")?;
    let supers: TokenStream = "super::".parse().compile_err("Failed to init supers")?;

    let outputs = nested_fixtures(
        path
            .read_dir()
            .compile_error(fmt!("Failed to read {:?}", path))?
            .map(|result|
                result.compile_error(fmt!("Failed to read in {:?}", path))
            ),
        &TokenStream::new(),
        &Params {
            tag,
            base_name,
            supers,
            actual_file_name,
            expected_file_name,
        }
    );

    // <String> panics come from the formatted panic!, including .unwrap/.expect
    // <&str> panics come from unformatted panic!, like panic!("Nooo!")
    Ok(quote! {
        fn #name #generic_lt #generic_params #generic_gt (mut #param_name: (
            impl std::ops::Fn(&mut std::option::Option<#param_type>) + std::panic::RefUnwindSafe + std::panic::UnwindSafe,
            &'static str,
            &'static str,
         )) #generic_where {
            #item

            let (to_call, (provider, expected_file, actual_file)) =
                (&#name, #param_name);

            let result = format!(
                "{:#?}\n",
                std::panic::catch_unwind(
                    move || {
                        let mut temp = std::option::Option::None;
                        provider(&mut temp);
                        to_call(temp.unwrap())
                    }
                ).map_err(|err| err
                    .downcast::<String>()
                    .or_else(|err|
                        if let Some(string) = err.downcast_ref::<&str>() {
                            std::result::Result::Ok(std::boxed::Box::new(string.to_string()))
                        } else {
                            std::result::Result::Err(("<!String> Panic", err))
                        }
                    )
                    .map(|ok| ("<String> Panic", ok))
                )
            );
            if std::path::Path::new(expected_file).is_file() {
                let expected = std::fs::read_to_string(expected_file)
                    .unwrap_or_else(|err|
                        panic!("Reading expected from {}: {:?}", expected_file, err)
                    );
                assert_eq!(result, expected)
            } else {
                std::fs::write(actual_file, result.as_bytes())
                    .unwrap_or_else(|err|
                        panic!("Writing actual to {}: {:?}", actual_file, err)
                    );
                panic!("No expected value set: {}", actual_file)
            }
        }

        mod #name {
            #outputs
        }
    })
}

fn pull_function_description(item: TokenStream) -> Result<(Ident, Generics, (Ident, Type)), TokenStream> {
    let Signature {
        ident: name,
        inputs: param,
        generics,
        ..
    } = parse2::<ItemFn>(item.clone())
        .compile_error(fmt!("Expected attribute must be on a function, received: {}\n\n", item))?
        .sig;
    let param: SmallVec<[FnArg; 1]> = param.into_iter().collect();
    let param = match param.into_inner() {
        Ok([param]) => param,
        Err(ref param) if param.is_empty() => return ().compile_err("No input parameter"),
        Err(param) => return ().compile_error(fmt!(
            "Expected one parameter, received {}",
            param
                .into_iter()
                .map(FnArg::into_token_stream)
                .flatten()
                .collect::<TokenStream>()
        )),
    };
    let (param_type, param_name) = match param {
        FnArg::Typed(PatType { pat, ty, .. }) => (*ty, *pat),
        param => return ().compile_error(fmt!("Unexpected self in {}", param.into_token_stream())),
    };
    let param_name = match param_name {
        Pat::Ident(PatIdent { ident, .. }) => ident,
        pat => return ().compile_error(fmt!("Expected parameter, received {}", pat.into_token_stream())),
    };
    if format!("{}", param_name) == format!("{}", name) {
        return ().compile_error(fmt!("Function {} may not share name with its parameter", name));
    }
    Ok((name, generics, (param_name, param_type)))
}

struct Params {
    tag: TokenStream,
    base_name: TokenStream,
    supers: TokenStream,
    actual_file_name: String,
    expected_file_name: String,
}

fn nested_fixtures(
    folders: impl IntoIterator<Item=Result<DirEntry, TokenStream>>,
    super_chain: &TokenStream,
    params: &Params,
) -> TokenStream {
    let Params {
        tag,
        base_name,
        supers,
        actual_file_name,
        expected_file_name,
    } = params;
    let super_chain = {
        let mut super_chain = super_chain.clone();
        supers.to_tokens(&mut super_chain);
        super_chain
    };
    folders
        .into_iter()
        .map(|result| result.and_then(|fixture: DirEntry| {
            let fixture_path = fixture
                .path()
                .canonicalize()
                .compile_error(fmt!("Failed to canonicalize fixtures: {:?}", fixture))?;
            let fixture_name = parse_str::<Ident>(fixture
                .file_name()
                .to_str()
                .compile_error(fmt!("Failed to convert filename to utf8 of {:?}", fixture))?,
            ).compile_error(fmt!("Failed to convert filename of {:?} into rust identifier", fixture_path))?;

            let mut input_rs = None;
            let mut input_txt = None;
            let mut input_bin = None;
            let mut folders: Option<Vec<_>> = None;
            for file in fixture_path
                .read_dir()
                .compile_error(fmt!("Failed to read fixture directory {:?}", fixture_path))?
            {
                macro_rules! push_err {($ex:expr) => {{
                    match $ex {
                        Err(e) => {
                            folders.get_or_insert_with(Vec::new).push(Err(e));
                            continue;
                        },
                        Ok(value) => value,
                    }
                }};}

                let file: DirEntry = push_err!(
                    file.compile_error(fmt!("Failed to get DirEntry in {:?}", fixture_path))
                );

                if push_err!(
                    file.file_type().compile_error(fmt!("Bad file type of {:?}", file))
                ).is_dir() {
                    folders
                        .get_or_insert_with(Vec::new)
                        .push(Ok(file));
                    continue;
                }

                let name = file.file_name();
                let name = push_err!(
                    name.to_str().compile_error(fmt!("Unresolvable file name"))
                );

                let file_pointer = match name {
                    INPUT_RS => &mut input_rs,
                    INPUT_TXT => &mut input_txt,
                    INPUT_BIN => &mut input_bin,
                    _ => continue,
                };
                *file_pointer = Some(file);
            }

            match (
                folders.as_ref().map_or(
                    true,
                    |folders|
                        folders.iter().any(Result::is_err),
                ),
                &input_rs,
                &input_bin,
                &input_txt,
            ) {
                // No vec and one file
                // Vec with error and one file
                (true, None, Some(_), None) => {},
                (true, None, None, Some(_)) => {},
                (true, Some(_), None, None) => {},
                // Vec without errors and no files
                (false, None, None, None) => {},
                // Vec with error and multiple files
                // Vec with error and no files
                // No vec and no files
                // No vec and multiple files
                _ => folders
                    .get_or_insert_with(Vec::new)
                    .push(().compile_error(fmt!(
                        "Expected sub-directories or exactly one of {}, {}, or {} in {:?}",
                        INPUT_RS,
                        INPUT_BIN,
                        INPUT_TXT,
                        fixture_path,
                    ))),
            }

            let (include, file) = match (folders, input_rs, input_bin, input_txt) {
                // dir
                (Some(folders), _, _, _) => {
                    let fixtures = nested_fixtures(
                        folders,
                        &super_chain,
                        params,
                    );
                    return Ok(quote! {
                        mod #fixture_name {
                            #fixtures
                        }
                    })
                },
                // rs
                (None, Some(file), None, None) => ("include", file),
                // bin
                (None, None, Some(file), None) => ("include_bytes", file),
                // txt
                (None, None, None, Some(file)) => ("include_str", file),
                // If there wasn't a single-file, folders would be populated
                _ => unreachable!(),
            };
            // Can't panic; we have them explicitly outlined
            let include= Ident::new(include, Span::call_site());

            let make_literal = |path: PathBuf| path
                .to_str()
                .compile_error(fmt!("Failed to get utf8 string from {:?}", path))
                .map(Literal::string);
            let input_literal = make_literal(file.path())?;
            let actual_literal = make_literal(fixture_path.join(actual_file_name))?;
            let expected_literal = make_literal(fixture_path.join(expected_file_name))?;

            Ok(quote! {
                #tag
                fn #fixture_name() {
                    #super_chain #base_name((
                        |#fixture_name: &mut std::option::Option<_>| {
                            #fixture_name.replace(#include!(#input_literal));
                        },
                        #expected_literal,
                        #actual_literal,
                    ))
                }
            })
        }))
        .map(EitherResult::either)
        .collect()
}
