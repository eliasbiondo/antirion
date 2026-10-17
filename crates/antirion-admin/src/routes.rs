//! The Admin API's operations by path, below `/admin/v1/`.
//!
//! An unoptimised build gives every future a function builds a stack slot of its own, whether or
//! not that branch runs. So the table is a chain of small synchronous functions, one per group of
//! operations, each building and boxing only the future of the operation it picks: no frame that
//! stays on the stack while a handler runs holds the futures of the operations it did not pick, and
//! a new operation adds to its group's short-lived frame only.

use std::future::{Future, ready};
use std::pin::Pin;

use antirion_config::model;
use axum::response::Response;
use http::Method;

use crate::call::Call;
use crate::error::AdminError;
use crate::{
    access_requests, access_routes, analytics, budget_routes, budgets, cache, cache_routes, catalog,
    catalog_read, change_requests, danger_routes, delivery, device, document, export, guardtest, health,
    invitations, keys, logfeedback, logs, mcp, me, mfa, models, nodes, oidc, ops, playground, pressure,
    pricing, provider_ops, push, recost, resources, role_bindings, runtime, safety_routes, saml, scim,
    session, sessions, settings, simulate, spend, tail, testmode, users, views,
};

/// An operation's future, boxed where it is built.
pub(crate) type Handled<'a> = Pin<Box<dyn Future<Output = Result<Response, AdminError>> + Send + 'a>>;

/// An answer known without waiting, as a [`Handled`].
pub(crate) fn done<'a>(result: Result<Response, AdminError>) -> Handled<'a> {
    Box::pin(ready(result))
}

fn not_found<'a>() -> Handled<'a> {
    done(Err(AdminError::not_found()))
}

/// The operation `segs` names; the groups are tried in order, as one table.
pub(crate) fn handle<'a>(call: &'a Call, segs: &'a [&'a str]) -> Handled<'a> {
    documents(call, segs)
        .or_else(|| identity(call, segs))
        .or_else(|| records(call, segs))
        .or_else(|| runtime_state(call, segs))
        .or_else(|| families(call, segs))
        .or_else(|| collections(call, segs))
        .unwrap_or_else(not_found)
}

/// Status, the configuration document, audit, request records, analytics and test mode.
fn documents<'a>(call: &'a Call, segs: &'a [&'a str]) -> Option<Handled<'a>> {
    Some(match segs {
        ["status"] => Box::pin(ops::status(call)),
        ["openapi.json"] => done(ops::openapi(call)),
        ["health"] => Box::pin(ops::health(call)),
        ["config"] => done(document::export(call)),
        ["config:apply"] => Box::pin(document::apply(call)),
        ["config:validate"] => Box::pin(document::validate(call)),
        ["config:diff"] => Box::pin(document::diff(call)),
        ["config", "schema"] => done(document::schema(call)),
        ["audit"] => Box::pin(ops::audit_list(call)),
        ["audit", id] => Box::pin(ops::audit_entry(call, id)),
        ["logs"] if call.method == Method::DELETE => Box::pin(logfeedback::erase(call)),
        ["logs"] => Box::pin(logs::list(call)),
        ["logs:tail"] => done(tail::tail(call)),
        ["logs:export"] => done(export::export(call)),
        ["analytics", "usage"] => Box::pin(analytics::usage(call)),
        ["analytics", "latency_histogram"] => Box::pin(analytics::latency_histogram(call)),
        ["provider-types"] => done(catalog::provider_types(call)),
        ["deployments", name, "capabilities"] => done(catalog::capabilities(call, name)),
        ["runtime", "deployments"] => done(health::list(call)),
        ["runtime", "deployments", name] => Box::pin(health::one(call, name)),
        ["runtime", "providers", provider, "keys", action] => {
            Box::pin(health::reset_key(call, provider, action))
        }
        ["routes", route, "state"] => done(health::route_state(call, route)),
        ["routes", route, "affinity"] => Box::pin(health::route_affinity(call, route)),
        ["test", action] => Box::pin(testmode::handle(call, action)),
        _ => return None,
    })
}

/// Sign-in settings, the caller's own account, sessions, invitations and role bindings.
fn identity<'a>(call: &'a Call, segs: &'a [&'a str]) -> Option<Handled<'a>> {
    Some(match segs {
        ["settings", "smtp:test"] => Box::pin(settings::smtp_test(call)),
        ["settings", "auth:rotate_scim_token"] => Box::pin(scim::rotate(call)),
        ["settings", "security:rotate_secrets"] => danger_routes::dispatch(call, segs),
        ["auth", "scim", "status"] => Box::pin(scim::status(call)),
        ["auth", "oidc", name, "login"] => Box::pin(oidc::login_test(call, name)),
        ["auth", "saml", name, "login"] => Box::pin(saml::login_test(call, name)),
        ["auth", "saml", target] => match target.split_once(':') {
            None => Box::pin(saml::status(call, target)),
            Some((name, "resync")) => Box::pin(saml::resync(call, name)),
            Some(_) => not_found(),
        },
        ["settings", key] => Box::pin(settings::handle(call, key)),
        ["auth", "me"] => Box::pin(session::whoami(call)),
        ["auth", "me", "preferences"] => Box::pin(me::prefs(call)),
        ["auth", "me:change_password"] => Box::pin(me::change_password(call)),
        ["auth", "me", "mfa"] => Box::pin(mfa::handle(call, &[])),
        ["auth", "me", "mfa:regenerate_recovery_codes"] => {
            Box::pin(mfa::handle(call, &[":regenerate_recovery_codes"]))
        }
        ["auth", "me", "mfa", factor] => Box::pin(mfa::handle(call, std::slice::from_ref(factor))),
        ["auth", "me", "tokens"] => Box::pin(me::tokens(call)),
        ["auth", "me", "push"] => Box::pin(push::overview(call)),
        ["auth", "me", "push:test"] => Box::pin(push::test(call)),
        ["auth", "me", "push", "subscriptions"] => Box::pin(push::subscriptions(call)),
        ["auth", "me", "push", "subscriptions", id] => Box::pin(push::subscription(call, id)),
        ["auth", "me", "tokens", kid] => Box::pin(me::token(call, kid)),
        ["auth", "sessions"] => Box::pin(sessions::mine(call, None)),
        ["auth", "sessions:revoke_others"] => Box::pin(sessions::mine(call, Some(":revoke_others"))),
        ["auth", "sessions", id] => Box::pin(sessions::mine(call, Some(id))),
        ["users", user, "sessions"] => Box::pin(sessions::of_user(call, user)),
        ["users", action] if action.ends_with(":reset_mfa") => {
            Box::pin(users::reset_mfa(call, action.trim_end_matches(":reset_mfa")))
        }
        ["invitations"] => Box::pin(invitations::collection(call)),
        ["invitations", segment] => Box::pin(invitations::one(call, segment)),
        ["auth", "logout"] => Box::pin(session::logout(call)),
        ["auth", action] if action.starts_with("device:") => {
            device::decide_boxed(call, action.trim_start_matches("device:"))
        }
        ["role_bindings"] => Box::pin(role_bindings::collection(call)),
        ["role_bindings", id] => Box::pin(role_bindings::one(call, id)),
        ["roles"] => done(role_bindings::roles(call)),
        _ => return None,
    })
}

/// Saved views, replays, keys' actions, users' tokens and the cluster.
fn records<'a>(call: &'a Call, segs: &'a [&'a str]) -> Option<Handled<'a>> {
    Some(match segs {
        ["logs", "schema"] => done(logs::schema(call)),
        ["logs", "views"] => Box::pin(views::collection(call)),
        ["logs", action] if action.ends_with(":replay") => {
            Box::pin(playground::replay(call, action.trim_end_matches(":replay")))
        }
        ["playground:send"] => Box::pin(playground::playground_send(call)),
        ["logs", "views", id] => Box::pin(views::one(call, id)),
        ["logs", id] => Box::pin(logs::get(call, id)),
        ["keys", action] if action.contains(':') => match action.split_once(':') {
            Some((name, "rotate")) => Box::pin(keys::rotate(call, name)),
            _ => not_found(),
        },
        ["users", user, "tokens"] => Box::pin(users::tokens(call, user)),
        ["cluster"] => Box::pin(nodes::cluster(call)),
        ["cluster", "converged"] => Box::pin(nodes::converged(call)),
        ["runtime", "nodes"] => Box::pin(nodes::list(call)),
        ["runtime", "nodes", segment] => Box::pin(nodes::one(call, segment)),
        _ => return None,
    })
}

/// Runtime readings, the cache, budgets, spend and prices, and the actions of providers, routes and
/// Models.
fn runtime_state<'a>(call: &'a Call, segs: &'a [&'a str]) -> Option<Handled<'a>> {
    Some(match segs {
        ["runtime", "providers"] => done(runtime::providers(call)),
        ["runtime", "plugins"] => done(runtime::plugins(call)),
        ["runtime", "plugins", name] => done(runtime::plugin(call, name)),
        ["runtime", "providers", name] => done(runtime::provider(call, name)),
        ["users", user, "tokens", kid] => Box::pin(users::token(call, user, kid)),
        ["mcp", "servers", name, "status"] => done(mcp::status(call, name)),
        ["mcp", "servers", action] if action.ends_with(":approve_tools") => Box::pin(mcp::approve_tools(
            call,
            action.trim_end_matches(":approve_tools"),
        )),
        ["runtime", "mcp", "sessions"] => done(mcp::sessions(call)),
        ["runtime", "cache"] => Box::pin(cache::status(call)),
        ["cache:invalidate"] => Box::pin(cache::invalidate(call)),
        ["runtime", "budgets"] => Box::pin(budgets::list(call)),
        ["runtime", "budgets", level, owner, name] => Box::pin(budgets::one(call, level, owner, name)),
        ["reports", "spend", "requests", id] => Box::pin(spend::request(call, id)),
        ["reports", "spend", "ledger"] => Box::pin(spend::ledger(call)),
        ["reports", "spend", "summary"] => Box::pin(spend::summary(call)),
        ["pricing", "deployments", name] => done(spend::price(call, name)),
        ["pricing", "deployments"] => done(pricing::list(call)),
        ["guardrails", action] if action.ends_with(":test") => {
            Box::pin(guardtest::run(call, action.trim_end_matches(":test")))
        }
        ["routing", "tiers"] => done(models::tiers(call)),
        ["change_requests"] => Box::pin(change_requests::collection(call)),
        ["change_requests", segment] => Box::pin(change_requests::one(call, segment)),
        ["providers", action] if action.ends_with(":test") => {
            Box::pin(provider_ops::test(call, action.trim_end_matches(":test")))
        }
        ["providers", action] if action.ends_with(":rotate_credential") => Box::pin(provider_ops::rotate(
            call,
            action.trim_end_matches(":rotate_credential"),
        )),
        ["routes", action] if action.ends_with(":simulate") => {
            Box::pin(simulate::simulate(call, action.trim_end_matches(":simulate")))
        }
        ["models", model, "access_requests"] => Box::pin(access_requests::collection(call, model)),
        ["models", model, "access_requests", segment] => {
            Box::pin(access_requests::action(call, model, segment))
        }
        ["catalog"] => done(catalog_read::get(call)),
        _ => return None,
    })
}

/// The families that route their own operations.
fn families<'a>(call: &'a Call, segs: &'a [&'a str]) -> Option<Handled<'a>> {
    Some(match segs {
        _ if delivery::routes(segs) => delivery::dispatch(call, segs),
        _ if access_routes::routes(segs) => access_routes::dispatch(call, segs),
        _ if safety_routes::routes(segs) => safety_routes::dispatch(call, segs),
        _ if budget_routes::routes(segs) => budget_routes::dispatch(call, segs),
        _ if cache_routes::routes(segs) => cache_routes::dispatch(call, segs),
        _ if danger_routes::routes(segs) => danger_routes::dispatch(call, segs),
        _ if recost::routes(segs) => Box::pin(recost::dispatch(call, segs)),
        _ if pressure::routes(segs) => pressure::dispatch(call, segs),
        _ => return None,
    })
}

/// The configuration's collections and their resources.
fn collections<'a>(call: &'a Call, segs: &'a [&'a str]) -> Option<Handled<'a>> {
    Some(match segs {
        [c] => match model::collection(c) {
            Some(def) => Box::pin(resources::collection(call, def)),
            None => not_found(),
        },
        [a, b] => match (model::collection(&format!("{a}/{b}")), model::collection(a)) {
            (Some(def), _) => Box::pin(resources::collection(call, def)),
            (None, Some(def)) if !(def.name == "keys" && b.contains(':')) => {
                Box::pin(resources::resource(call, def, b))
            }
            _ => not_found(),
        },
        [a, b, name] => match model::collection(&format!("{a}/{b}")) {
            Some(def) => Box::pin(resources::resource(call, def, name)),
            None => not_found(),
        },
        _ => return None,
    })
}
