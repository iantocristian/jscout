use std::collections::HashSet;
use std::path::Path;

use anyhow::Result;

use super::extract;

fn sites(source: &str) -> Result<Vec<super::EntitySite>> {
    crate::parse::with_parsed(source, Path::new("fixture.ts"), |ret, _| {
        extract(&ret.program, &HashSet::new())
    })
}

fn sites_with_exports(
    source: &str,
    exported: impl IntoIterator<Item = &'static str>,
) -> Result<Vec<super::EntitySite>> {
    let exported = exported.into_iter().map(str::to_string).collect();
    crate::parse::with_parsed(source, Path::new("fixture.ts"), |ret, _| {
        extract(&ret.program, &exported)
    })
}

#[test]
fn extracts_registry_registration_and_dispatch() -> Result<()> {
    let extracted = sites(
        "const TARGET = 'logic-id';\n\
         export const handler = () => {};\n\
         export default defineLogicFunction({ universalIdentifier: TARGET, handler });\n\
         export const route = () => ({ targetLogicFunctionUniversalIdentifier: TARGET });\n",
    )?;
    assert!(extracted.iter().any(|site| {
        site.entity_type == "registry"
            && site.role == "registered_handler"
            && site.identity_kind == "reference"
            && site.identity_name == "TARGET"
            && site.target_name.as_deref() == Some("handler")
    }));
    assert!(extracted.iter().any(|site| {
        site.entity_type == "registry"
            && site.role == "dispatch_site"
            && site.identity_name == "TARGET"
    }));
    Ok(())
}

#[test]
fn joins_database_event_listener_and_graphql_mutation_by_resource() -> Result<()> {
    let extracted = sites(
        "const OBJECT = 'slackAssistantRequest';\n\
         const worker = () => {};\n\
         defineLogicFunction({\n\
           universalIdentifier: 'worker-id',\n\
           handler: worker,\n\
           databaseEventTriggerSettings: { eventName: `${OBJECT}.created` },\n\
         });\n\
         client.mutation({ createSlackAssistantRequest: { id: true } });\n",
    )?;
    let roles: Vec<(&str, &str)> = extracted
        .iter()
        .filter(|site| site.entity_type == "data_lifecycle")
        .map(|site| (site.role, site.identity_name.as_str()))
        .collect();
    assert_eq!(
        roles,
        [
            ("lifecycle_listener", "slackAssistantRequest.created"),
            ("lifecycle_producer", "slackAssistantRequest.created"),
        ]
    );
    Ok(())
}

#[test]
fn extracts_queue_workers_producers_decorators_and_di_providers() -> Result<()> {
    let extracted = sites(
        "const TOKEN = Symbol('token');\n\
         const queueWorker = (job) => job.run();\n\
         new Worker('email', queueWorker);\n\
         emailQueue.add('email', payload);\n\
         const providers = [{ provide: TOKEN, useClass: EmailService }];\n\
         class Jobs {\n\
           @Process('email') run() {}\n\
           constructor(@Inject(TOKEN) service) {}\n\
         }\n",
    )?;
    assert!(extracted.iter().any(|site| {
        site.entity_type == "job"
            && site.role == "job_handler"
            && site.identity_name == "email"
            && site.target_name.as_deref() == Some("queueWorker")
    }));
    assert!(extracted.iter().any(|site| {
        site.entity_type == "job" && site.role == "job_producer" && site.identity_name == "email"
    }));
    assert!(extracted.iter().any(|site| {
        site.entity_type == "di_token"
            && site.role == "provider"
            && site.identity_name == "TOKEN"
            && site.target_name.as_deref() == Some("EmailService")
    }));
    assert!(extracted.iter().any(|site| {
        site.entity_type == "di_token"
            && site.role == "injection_site"
            && site.identity_name == "TOKEN"
    }));
    Ok(())
}

#[test]
fn extracts_twenty_this_qualified_queue_and_cron_calls() -> Result<()> {
    let extracted = sites(
        "class Producer {\n\
           enqueue(payload) {\n\
             this.messageQueueService.add(EmailJob.name, payload);\n\
           }\n\
           schedule() {\n\
             this.messageQueueService.addCron({\n\
               jobName: CleanupJob.name,\n\
               data: undefined,\n\
               options: { repeat: { pattern: '0 0 * * *' } },\n\
             });\n\
           }\n\
         }\n\
         class Consumer {\n\
           @Process(EmailJob.name) handleEmail() {}\n\
           @Process(CleanupJob.name) handleCleanup() {}\n\
         }\n",
    )?;

    for (identity, method) in [("EmailJob", "add"), ("CleanupJob", "addCron")] {
        assert!(extracted.iter().any(|site| {
            site.entity_type == "job"
                && site.role == "job_producer"
                && site.identity_kind == "reference"
                && site.identity_name == identity
                && site.detail["object"] == "this.messageQueueService"
                && site.detail["method"] == method
        }));
        assert!(extracted.iter().any(|site| {
            site.entity_type == "job"
                && site.role == "job_handler"
                && site.identity_kind == "reference"
                && site.identity_name == identity
        }));
    }
    Ok(())
}

#[test]
fn extracts_contract_declarations_exported_api_types_decorators_and_schemas() -> Result<()> {
    let extracted = sites_with_exports(
        "export interface User extends Entity { id: string }\n\
         export type UserResult = Promise<User>;\n\
         export enum UserState { Active }\n\
         export function load(input: User): Promise<UserResult> { throw Error(); }\n\
         export const save = (input: User): UserResult => input;\n\
         export const userSchema = z.object({ id: z.string() });\n\
         @InputType() class CreateUserDto { @IsString() name: string; }\n",
        [
            "User",
            "UserResult",
            "UserState",
            "load",
            "save",
            "userSchema",
        ],
    )?;
    for (entity_type, name) in [
        ("interface", "User"),
        ("type_alias", "UserResult"),
        ("enum", "UserState"),
        ("schema", "userSchema"),
        ("schema", "CreateUserDto"),
    ] {
        assert!(extracted.iter().any(|site| {
            site.plane == "contract"
                && site.role == "contract_declaration"
                && site.entity_type == entity_type
                && site.identity_name == name
        }));
    }
    assert!(
        extracted
            .iter()
            .any(|site| { site.role == "parameter_contract" && site.identity_name == "User" })
    );
    assert!(
        extracted
            .iter()
            .any(|site| { site.role == "return_contract" && site.identity_name == "UserResult" })
    );
    assert!(!extracted.iter().any(|site| {
        matches!(
            site.role,
            "parameter_contract" | "return_contract" | "contract_reference"
        ) && site.identity_name == "Promise"
    }));
    assert!(
        extracted
            .iter()
            .any(|site| { site.role == "decorator_use" && site.identity_name == "InputType" })
    );
    assert!(
        extracted
            .iter()
            .any(|site| { site.role == "decorator_use" && site.identity_name == "IsString" })
    );
    Ok(())
}

#[test]
fn excludes_scoped_generic_parameters_from_contract_references() -> Result<()> {
    let extracted = sites_with_exports(
        "export interface Page<T extends Entity> { value: T; next: Page<T> }\n\
         export type Mapper<T> = <U>(value: T, other: U) => Pair<T, U>;\n\
         export function pick<T>(value: T): T { return value; }\n\
         export const identity = <T>(value: T): T => value;\n\
         export class Box<T> { map<U>(value: T, fn: (item: T) => U): U { throw Error(); } }\n",
        ["Page", "Mapper", "pick", "identity", "Box"],
    )?;

    let references: Vec<&str> = extracted
        .iter()
        .filter(|site| {
            matches!(
                site.role,
                "parameter_contract" | "return_contract" | "contract_reference"
            )
        })
        .map(|site| site.identity_name.as_str())
        .collect();
    assert!(!references.iter().any(|name| matches!(*name, "T" | "U")));
    assert!(references.contains(&"Entity"));
    assert!(references.contains(&"Page"));
    assert!(references.contains(&"Pair"));
    Ok(())
}

#[test]
fn extracts_routes_graphql_env_database_flags_and_external_hosts() -> Result<()> {
    let extracted = sites(
        "@Controller('/users')\n\
         class UsersController {\n\
           @Get(':id') getUser() {}\n\
           @Mutation('saveUser') saveUser() {}\n\
         }\n\
         router.post('/jobs', createJob);\n\
         client.query({ currentUser: { id: true } });\n\
         const apiKey = process.env.API_KEY;\n\
         const region = process.env['REGION'];\n\
         const token = Deno.env.get('TOKEN');\n\
         prisma.user.findMany();\n\
         prisma.user.create({ data });\n\
         this.repository.save(data);\n\
         flags.isEnabled('new-ui');\n\
         fetch('https://api.example.com/v1/users');\n",
    )?;
    for (entity_type, role, identity) in [
        ("route", "route_handler", "GET /users/:id"),
        ("route", "route_handler", "POST /jobs"),
        ("graphql_operation", "graphql_handler", "mutation:saveUser"),
        (
            "graphql_operation",
            "graphql_operation",
            "query:currentUser",
        ),
        ("environment_variable", "environment_read", "API_KEY"),
        ("environment_variable", "environment_read", "REGION"),
        ("environment_variable", "environment_read", "TOKEN"),
        ("database_resource", "database_read", "user"),
        ("database_resource", "database_write", "user"),
        ("feature_flag", "feature_flag_check", "new-ui"),
        ("external_host", "external_host_call", "api.example.com"),
    ] {
        assert!(
            extracted.iter().any(|site| {
                site.plane == "general"
                    && site.entity_type == entity_type
                    && site.role == role
                    && site.identity_name == identity
            }),
            "missing {entity_type}/{role}/{identity}"
        );
    }
    assert!(!extracted.iter().any(|site| {
        site.entity_type == "database_resource" && site.identity_name == "repository"
    }));
    Ok(())
}

#[test]
fn extracts_named_routers_without_treating_graphql_options_as_operations() -> Result<()> {
    let extracted = sites(
        "usersRouter.get('/users', listUsers);\n\
         client.query({ query: GET_USER, variables: { id: 1 }, fetchPolicy: 'cache-first' });\n\
         client.query({ currentUser: { id: true } });\n",
    )?;

    assert!(extracted.iter().any(|site| {
        site.entity_type == "route"
            && site.identity_name == "GET /users"
            && site.target_name.as_deref() == Some("listUsers")
    }));
    assert!(extracted.iter().any(|site| {
        site.entity_type == "graphql_operation" && site.identity_name == "query:currentUser"
    }));
    assert!(!extracted.iter().any(|site| {
        site.entity_type == "graphql_operation"
            && matches!(
                site.identity_name.as_str(),
                "query:query" | "query:variables" | "query:fetchPolicy"
            )
    }));
    Ok(())
}

#[test]
fn extracts_qualified_database_holders_and_labels_handle_acquisition() -> Result<()> {
    let extracted = sites(
        "this.db.insert(users);\n\
         ctx.db.insert(accounts);\n\
         this.userRepository.save(data);\n\
         this.InvoiceModel.findMany();\n\
         this.repository.save(data);\n\
         getRepository(User);\n",
    )?;

    for (resource, role) in [
        ("users", "database_write"),
        ("accounts", "database_write"),
        ("user", "database_write"),
        ("invoice", "database_read"),
        ("User", "database_acquire"),
    ] {
        assert!(
            extracted.iter().any(|site| {
                site.entity_type == "database_resource"
                    && site.identity_name == resource
                    && site.role == role
            }),
            "missing {resource}/{role}"
        );
    }
    assert!(!extracted.iter().any(|site| {
        site.entity_type == "database_resource"
            && matches!(site.identity_name.as_str(), "repository" | "data")
    }));
    Ok(())
}

#[test]
fn separates_configuration_keys_from_environment_variables() -> Result<()> {
    let extracted = sites(
        "config.get('database.host');\n\
         this.configService.get('PORT');\n\
         Deno.env.get('TOKEN');\n",
    )?;

    for key in ["database.host", "PORT"] {
        assert!(extracted.iter().any(|site| {
            site.entity_type == "config_key"
                && site.role == "config_read"
                && site.identity_name == key
        }));
        assert!(!extracted.iter().any(|site| {
            site.entity_type == "environment_variable" && site.identity_name == key
        }));
    }
    assert!(extracted.iter().any(|site| {
        site.entity_type == "environment_variable" && site.identity_name == "TOKEN"
    }));
    Ok(())
}
