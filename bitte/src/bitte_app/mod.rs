mod certs;
mod info;
mod provision;
mod rebuild;
mod ssh;
mod terraform;
mod types;

use clap::ArgMatches;
use execute::Execute;
use restson::RestClient;
use shellexpand::tilde;
use std::fs::File;
use std::process::Command;
use std::{env, error::Error};
use std::{fmt, path::Path, process::Stdio};
use std::{io::BufReader, time::Duration};
use tokio::{net::TcpStream, time::timeout};

use self::{
    info::{asg_info, cli_info_print, instance_info},
    terraform::current_state_version,
    types::{HttpWorkspace, HttpWorkspaceState, HttpWorkspaceStateValue, TerraformCredentialFile},
};

pub(crate) async fn cli_certs(sub: &ArgMatches) {
    certs::cli_certs(sub).await
}

pub(crate) async fn cli_provision(sub: &ArgMatches) {
    provision::cli_provision(sub).await
}

pub(crate) async fn cli_ssh(sub: &ArgMatches) {
    ssh::cli_ssh(sub).await
}

pub(crate) async fn cli_rebuild(sub: &ArgMatches) {
    rebuild::cli_rebuild(sub).await
}

pub(crate) async fn cli_info(_sub: &ArgMatches) {
    let info = fetch_current_state_version("clients")
        .or_else(|_| fetch_current_state_version("core"))
        .expect("Coudln't fetch clients or core workspaces");
    cli_info_print(info).await;
}

pub(crate) async fn cli_tf(sub: &ArgMatches) {
    let workspace: String = sub
        .value_of_t("workspace")
        .expect("workspace argument is missing");

    match sub.subcommand() {
        Some(("plan", sub_sub)) => terraform::cli_tf_plan(workspace, sub_sub).await,
        Some(("apply", sub_sub)) => terraform::cli_tf_apply(workspace, sub_sub).await,
        Some(("workspaces", sub_sub)) => terraform::cli_tf_workspaces(workspace, sub_sub).await,
        _ => println!("Unknown command"),
    }
}

fn bitte_cluster() -> String {
    env::var("BITTE_CLUSTER").expect("BITTE_CLUSTER environment variable must be set")
}

fn handle_command_error(mut command: std::process::Command) -> Result<String, ExeError> {
    println!("running: {:?}", command);
    // command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());

    match command.execute_output() {
        Ok(output) => match output.status.code() {
            Some(exit_code) => {
                if exit_code == 0 {
                    Ok("Ok".to_string())
                } else {
                    Err(ExeError {
                        details: String::from_utf8_lossy(&output.stderr).to_string(),
                    })
                }
            }
            None => Err(ExeError {
                details: "interrupted".to_string(),
            }),
        },
        Err(e) => Err(ExeError {
            details: e.to_string(),
        }),
    }
}

#[derive(Debug)]
struct ExeError {
    details: String,
}

impl fmt::Display for ExeError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.details)
    }
}

impl Error for ExeError {
    fn description(&self) -> &str {
        &self.details
    }
}

fn fetch_current_state_version(workspace_name_suffix: &str) -> Result<String, Box<dyn Error>> {
    let terraform_organization = terraform_organization();
    let workspace_name = format!("{}_{}", bitte_cluster(), workspace_name_suffix);
    let workspace_id = workspace_id(terraform_organization.as_str(), workspace_name.as_str())?;
    current_state_version(&workspace_id)
}

fn current_state_version_output(state_id: &str) -> Result<HttpWorkspaceStateValue, Box<dyn Error>> {
    let mut client = terraform_client();
    let current_state_version_output: Result<HttpWorkspaceState, restson::Error> =
        client.get(state_id);
    match current_state_version_output {
        Ok(output) => Ok(output.data.attributes.value),
        Err(e) => Err(e.into()),
    }
}

fn workspace_id(organization: &str, workspace: &str) -> Result<String, Box<dyn Error>> {
    let mut client = terraform_client();
    let params = (organization, workspace);
    let workspace: Result<HttpWorkspace, restson::Error> = client.get(params);
    match workspace {
        Ok(workspace) => Ok(workspace.data.id),
        Err(e) => Err(e.into()),
    }
}

fn terraform_client() -> RestClient {
    let mut client =
        RestClient::new("https://app.terraform.io").expect("Couldn't create RestClient");
    let token =
        terraform_token().expect("Make sure you are logged into terraform: run `terraform login`");
    client
        .set_header("Authorization", format!("Bearer {}", token).as_str())
        .expect("Coudln't set Authorization header");
    client
        .set_header("Content-Type", "application/vnd.api+json")
        .expect("Couldn't set Content-Type header");
    client
}

fn terraform_token() -> Result<String, Box<dyn Error>> {
    let creds = parse_terraform_credentials();
    let c = &creds.credentials["app.terraform.io"];
    let token = &c.token;
    Ok(token.to_string())
}

fn parse_terraform_credentials() -> TerraformCredentialFile {
    let exp = &tilde("~/.terraform.d/credentials.tfrc.json").to_string();
    let path = Path::new(exp);
    let file = File::open(path).expect(format!("Couldn't read {}", exp).as_str());
    let reader = BufReader::new(file);
    let creds: TerraformCredentialFile =
        serde_json::from_reader(reader).expect(format!("Couldn't parse {}", exp).as_str());
    creds
}

fn terraform_organization() -> String {
    env::var("TERRAFORM_ORGANIZATION")
        .expect("TERRAFORM_ORGANIZATION environment variable must be set")
}

async fn wait_for_ssh(ip: String) {
    let addr = format!("{}:22", ip);

    for i in 0..120 {
        let stream = TcpStream::connect(addr.clone());
        let t = timeout(Duration::from_millis(10000), stream);
        match t.await {
            Ok(o) => match o {
                Ok(_) => {
                    return;
                }
                Err(ee) => {
                    if i >= 120 {
                        println!("error while connecting: {}", ee);
                    }
                }
            },
            Err(e) => println!("Waiting for {} to respond: {}", addr, e),
        }
    }
}

fn check_cmd(cmd: &mut Command) {
    println!("run: {:?}", cmd);
    cmd.status()
        .expect(format!("failed to run: {:?}", cmd).as_str());
}

#[derive(Clone)]
struct Instance {
    public_ip: String,
    name: String,
    uid: String,
    flake_attr: String,
    s3_cache: String,
}

impl Instance {
    pub fn new(
        public_ip: String,
        name: String,
        uid: String,
        flake_attr: String,
        s3_cache: String,
    ) -> Instance {
        Instance {
            public_ip,
            name,
            uid,
            flake_attr,
            s3_cache,
        }
    }
}

async fn find_instance(needle: &str) -> Option<Instance> {
    println!("needle: {}", needle);
    match find_instances(vec![needle]).await.first() {
        Some(instance) => Some(instance.clone()),
        None => None,
    }
}

async fn find_instances(patterns: Vec<&str>) -> Vec<Instance> {
    let current_state_version = fetch_current_state_version("clients")
        .or_else(|_| fetch_current_state_version("core"))
        .expect("Coudln't fetch clients or core workspaces");

    let output = current_state_version_output(&current_state_version)
        .expect("Problem loading state version from terraform");

    let mut results = vec![];

    for instance in output.instances.values().into_iter() {
        if patterns.iter().any(|pattern| {
            [
                instance.private_ip.as_str(),
                instance.public_ip.as_str(),
                instance.name.as_str(),
            ]
            .contains(pattern)
        }) {
            results.push(Instance::new(
                instance.public_ip.to_string(),
                instance.name.to_string(),
                instance.uid.to_string(),
                instance.flake_attr.to_string(),
                output.s3_cache.to_string(),
            ));
        }
    }

    if let Some(asgs) = output.asgs {
        for (_, asg) in asgs {
            let asg_infos = asg_info(asg.arn.as_str(), asg.region.as_str()).await;
            for asg_info in asg_infos {
                let instance_infos =
                    instance_info(asg_info.instance_id.as_str(), asg.region.as_str()).await;
                for instance_info in instance_infos {
                    if patterns.iter().any(|pattern| {
                        [
                            instance_info.instance_id.as_ref(),
                            instance_info.public_dns_name.as_ref(),
                            instance_info.public_ip_address.as_ref(),
                            instance_info.private_dns_name.as_ref(),
                            instance_info.private_ip_address.as_ref(),
                        ]
                        .iter()
                        .map(|x| x.map_or_else(|| "", |y| y.as_str()))
                        .collect::<String>()
                        .contains(pattern)
                    }) {
                        if let Some(ip) = instance_info.public_ip_address {
                            results.push(Instance::new(
                                ip,
                                instance_info
                                    .instance_id
                                    .map_or_else(|| "".to_string(), |x| x),
                                asg.uid.clone(),
                                asg.flake_attr.clone(),
                                output.s3_cache.clone(),
                            ))
                        }
                    }
                }
            }
        }
    }

    results
}
