// policies.cedar — who may do what, and through which gateway. Nothing here says
// how narrowly the broker then executes it: timeouts and storage access live in
// broker.yaml's constraint sets. Editing this file can broaden who may act; it
// can never widen a timeout or reach storage a constraint set did not grant.
//
// chmod 0600, owner-owned, single-link. Validated in Cedar's strict mode at
// startup against a schema generated from broker.yaml's declared world, so a
// typo in a principal, provider, or capability name refuses startup instead of
// becoming a rule that silently never matches.
//
// The @id annotations are what audit records carry as `policy_ids`. Without
// them Cedar names policies positionally and inserting one renumbers the rest,
// which is exactly what an audit trail must not do.

// The session gate: may this person drive this agent at all, and through which
// gateway? Checked before any capability is authorized, so a sender the owner
// never permitted is refused before a model call is paid for.
@id("owner-may-prompt-pledge-keeper")
permit(principal == Dekopon::Principal::"owner",
       action == Dekopon::Action::"agent.prompt",
       resource == Dekopon::Agent::"pledge-keeper")
when { context has via && context.via == "dekopon-gatewayd" };

// Reads go to everyone: any mapped principal may list, check, and audit the
// ledger through the pledge:read-only action group, which holds exactly the
// ReadOnly capabilities (list, due, stats) and never the writes.
@id("everyone-may-read-pledges")
permit(principal,
       action in Dekopon::Action::"pledge:read-only",
       resource == Dekopon::Provider::"pledge")
when { context has agent && context.agent == "pledge-keeper"
    && context has via && context.via == "dekopon-gatewayd" };

// Writes stay with the owner: only mapped senders the owner names may make,
// keep, or drop promises.
@id("owner-may-write-pledges")
permit(principal == Dekopon::Principal::"owner",
       action in [Dekopon::Action::"pledge.make",
                  Dekopon::Action::"pledge.keep",
                  Dekopon::Action::"pledge.drop"],
       resource == Dekopon::Provider::"pledge")
when { context has agent && context.agent == "pledge-keeper"
    && context has via && context.via == "dekopon-gatewayd" };
