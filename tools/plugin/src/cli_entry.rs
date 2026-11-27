use crate::cli::{CliCommand, parse_args};
use crate::error::CliError;
use crate::{build_cmd, dev, pack, scaffold, sign, test_cmd, validate};

pub fn run(args: &[String]) -> Result<String, CliError> {
    match parse_args(args)? {
        CliCommand::Help(msg) => Ok(format!("{msg}\n")),
        CliCommand::Scaffold(scaffold_args) => scaffold::execute_scaffold(&scaffold_args),
        CliCommand::Validate { path, trust_key } => {
            validate::execute_validate_with_trust_key(&path, trust_key.as_deref())
        }
        CliCommand::Test { dir, offline } => test_cmd::execute_test(&dir, offline),
        CliCommand::Build { dir } => build_cmd::execute_build(&dir),
        CliCommand::Pack { dir, output } => pack::execute_pack(&dir, &output),
        CliCommand::Sign { package, key } => sign::execute_sign(&package, &key),
        CliCommand::Keygen { key, pubkey } => sign::execute_keygen(&key, &pubkey),
        CliCommand::Dev(dev_args) => dev::execute_dev(&dev_args),
    }
}
