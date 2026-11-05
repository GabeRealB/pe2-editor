//! A prefab as its asset holds it: a skeleton, the frame everything hangs in, and named parts. A part says where it hangs,
//! on a joint or on another part, and what fills it says the rest: a model, how it is held, what it overrides. What fills
//! a part may vary: a group of variants is what varies together, each of its members filling several parts, as an outfit
//! does a body and both hands. Which member is shown is the host's (`Project::chosen`), never written, so that combinations
//! are looked at, not stored. A part nothing fills is open: a place for what is chosen elsewhere, such as a weapon.

use pe2_ext::{Asset, Call, Project, Result, Style, Table, Ui, Value, anyhow, bail};

/// A part, as what fills it now.
#[derive(Clone, Debug, PartialEq)]
pub struct Filled {
    pub part: String,
    /// What it hangs on; none for what is stored and not shown.
    pub node: Option<String>,
    /// Where in the asset what fills it is, step by step: what writes to it starts there.
    pub within: Vec<String>,
    pub fill: Option<Value>,
}

/// A group of variants, and who is being looked at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub name: String,
    pub members: Vec<String>,
    pub chosen: String,
}

fn table<'a>(value: Option<&'a Value>, of: &str) -> Result<Option<&'a Table>> {
    value.map(|value| value.as_table().ok_or_else(|| anyhow!("{of} is no table"))).transpose()
}

/// The member shown of a group: the one chosen where it still exists, else the first.
#[must_use]
pub fn member_of(members: &[String], chosen: Option<String>) -> Option<String> {
    chosen.filter(|chosen| members.contains(chosen)).or_else(|| members.first().cloned())
}

/// What an asset's groups are.
#[must_use]
pub fn groups(asset: &Asset, project: &dyn Project) -> Vec<Group> {
    groups_as(asset, |group| project.chosen(asset.path(), group))
}

/// What an asset's groups are, each member chosen as `chosen` says of the group.
#[must_use]
pub fn groups_as(asset: &Asset, chosen: impl Fn(&str) -> Option<String>) -> Vec<Group> {
    let Some(groups) = asset.field("groups").and_then(Value::as_table) else { return Vec::new() };
    let group = |(name, members): (&String, &Value)| {
        let members: Vec<String> = members.as_table()?.keys().cloned().collect();
        let chosen = member_of(&members, chosen(name))?;
        Some(Group { name: name.clone(), members, chosen })
    };
    groups.iter().filter_map(group).collect()
}

/// Each part with what fills it, given the members shown.
///
/// # Errors
/// A part is filled twice, or a member fills a part there is not.
pub fn filled_from(parts: Option<&Table>, groups: Option<&Table>, shown: &[Group]) -> Result<Vec<Filled>> {
    let mut out = Vec::new();
    for (part, held) in parts.into_iter().flatten() {
        let node = held.get("node").and_then(Value::as_str).map(str::to_string);
        let fixed = held.get("model").is_some().then(|| (vec!["parts".to_string(), part.clone()], held.clone()));
        let mut found = fixed;
        for group in shown {
            let member = table(
                groups.and_then(|groups| groups.get(&group.name)).and_then(|members| members.get(&group.chosen)),
                &group.chosen,
            )?;
            let Some(fill) = member.and_then(|member| member.get(part)) else { continue };
            if found.is_some() {
                bail!("{part} is filled twice");
            }
            found = Some((
                vec!["groups".to_string(), group.name.clone(), group.chosen.clone(), part.clone()],
                fill.clone(),
            ));
        }
        let (within, fill) = found
            .map_or_else(|| (vec!["parts".to_string(), part.clone()], None), |(within, fill)| (within, Some(fill)));
        out.push(Filled { part: part.clone(), node, within, fill });
    }
    for group in shown {
        let member = groups
            .and_then(|groups| groups.get(&group.name))
            .and_then(|members| members.get(&group.chosen))
            .and_then(Value::as_table);
        if let Some(unknown) = member
            .into_iter()
            .flatten()
            .map(|(part, _)| part)
            .find(|part| !parts.is_some_and(|parts| parts.contains_key(*part)))
        {
            bail!("{} of {} fills {unknown}, which is no part", group.chosen, group.name);
        }
    }
    Ok(out)
}

/// Each part of a prefab with what fills it now.
///
/// # Errors
/// As `filled_from`, or its tables are not tables.
pub fn filled(asset: &Asset, project: &dyn Project) -> Result<Vec<Filled>> {
    filled_as(asset, &groups(asset, project))
}

/// Each part of a prefab with what fills it, with these groups shown.
///
/// # Errors
/// As `filled_from`, or its tables are not tables.
pub fn filled_as(asset: &Asset, shown: &[Group]) -> Result<Vec<Filled>> {
    filled_from(table(asset.field("parts"), "parts")?, table(asset.field("groups"), "groups")?, shown)
}

/// Every part a member of a group fills, by its address: `outfit/10300/body`.
#[must_use]
pub fn members(asset: &Asset) -> Vec<String> {
    addresses(asset.field("groups").and_then(Value::as_table))
}

fn addresses(groups: Option<&Table>) -> Vec<String> {
    let mut out = Vec::new();
    for (group, members) in groups.into_iter().flatten() {
        for (member, parts) in members.as_table().into_iter().flatten() {
            out.extend(parts.as_table().into_iter().flatten().map(|(part, _)| format!("{group}/{member}/{part}")));
        }
    }
    out
}

/// What a member of a group fills a part with, as an output of the prefab: a model, as this node holds it, so that what
/// reads it reads its overrides and pose too. A sub-path reads on from the prefab, told which (`tokens::leads_on`).
///
/// # Errors
/// It has no such member, or that member does not fill the part.
pub fn member(call: &mut Call) -> Result<Table> {
    let asked: String = call.parameter()?;
    let group = call.settings.get("each").and_then(Value::as_str).unwrap_or_default();
    let address = format!("{group}/{asked}");
    let fields = call.given();
    address
        .split('/')
        .fold(fields.get("groups"), |at, step| at?.get(step))
        .ok_or_else(|| anyhow!("it has no member {address}"))?;
    Ok(pe2_ext::tokens::leads_on(call.host.path(), "model", &[("member", &address)]))
}

/// What is at a path of an asset.
#[must_use]
pub fn fill_at<'a>(asset: &Asset<'a>, within: &[String]) -> Option<&'a Value> {
    let (first, steps) = within.split_first()?;
    steps.iter().try_fold(asset.field(first)?, |value, step| value.get(step))
}

/// A field's value with the table at a path of it changed.
#[must_use]
pub fn changed(mut root: Value, steps: &[String], change: impl FnOnce(&mut Table)) -> Option<Value> {
    let mut at = &mut root;
    for step in steps {
        at = at.as_table_mut()?.get_mut(step)?;
    }
    change(at.as_table_mut()?);
    Some(root)
}

/// The top-level field a change of what is at a path writes, and what it comes to.
#[must_use]
pub fn edited(asset: &Asset, within: &[String], change: impl FnOnce(&mut Table)) -> Option<(String, Value)> {
    let (first, steps) = within.split_first()?;
    Some((first.clone(), changed(asset.field(first)?.clone(), steps, change)?))
}

/// Whether a part hangs, however far down, from another: what would make a loop of moving the other onto it.
#[must_use]
pub fn hangs_from(parts: &[Filled], part: &str, from: &str) -> bool {
    let mut at = part;
    for _ in 0..=parts.len() {
        if at == from {
            return true;
        }
        match parts.iter().find(|filled| filled.part == at).and_then(|filled| filled.node.as_deref()) {
            Some(node) => at = node,
            None => return false,
        }
    }
    false
}

/// A row of a view's toolbar: a dropdown for each group with a choice to make.
pub fn bar(ui: &mut Ui) {
    let Some(asset) = ui.asset() else { return };
    let shown = groups(&asset, ui.project());
    for group in shown.iter().filter(|group| group.members.len() > 1) {
        ui.styled(&group.name, Style::Note);
        if let Some(chosen) = ui.combo(&format!("group {}", group.name), &group.chosen, &group.members) {
            ui.choose(&group.name, &group.members[chosen]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(text: &str) -> Table {
        text.parse().unwrap()
    }

    const PREFAB: &str = "
        [parts]
        body = {}
        hand = { node = 'wrist' }
        held = { node = 'hand' }
        ring = { node = 'hand', model = { slot = 'uid://ring' } }
        [groups.outfit.a]
        body = { model = { slot = 'uid://body_a' } }
        hand = { model = { slot = 'uid://hand_a' } }
        [groups.outfit.b]
        body = { model = { slot = 'uid://body_b' } }
    ";

    fn shown(member: &str) -> Vec<Group> {
        vec![Group {
            name: "outfit".to_string(),
            members: vec!["a".to_string(), "b".to_string()],
            chosen: member.to_string(),
        }]
    }

    fn fills(member: &str) -> Vec<Filled> {
        let held = parsed(PREFAB);
        filled_from(held["parts"].as_table(), held["groups"].as_table(), &shown(member)).unwrap()
    }

    fn of<'a>(all: &'a [Filled], part: &str) -> &'a Filled {
        all.iter().find(|filled| filled.part == part).unwrap()
    }

    #[test]
    fn a_member_fills_what_it_has_and_the_rest_is_open() {
        let a = fills("a");
        assert_eq!(of(&a, "hand").within, ["groups", "outfit", "a", "hand"]);
        assert!(of(&a, "held").fill.is_none(), "nothing fills it");
        assert_eq!(of(&a, "held").within, ["parts", "held"], "what would fill it goes on the part");
        assert!(of(&fills("b"), "hand").fill.is_none(), "b has no hand of its own");
    }

    #[test]
    fn a_part_can_be_filled_for_good() {
        assert_eq!(of(&fills("a"), "ring").within, ["parts", "ring"]);
        assert!(of(&fills("b"), "ring").fill.is_some());
    }

    #[test]
    fn a_part_filled_twice_or_a_part_there_is_not_says_so() {
        let twice = parsed("[parts]\nring = { model = 'uid://x' }\n[groups.outfit.a]\nring = { model = 'uid://y' }");
        assert!(filled_from(twice["parts"].as_table(), twice["groups"].as_table(), &shown("a")).is_err());
        let stray = parsed("[parts]\nbody = {}\n[groups.outfit.a]\nhat = { model = 'uid://y' }");
        assert!(filled_from(stray["parts"].as_table(), stray["groups"].as_table(), &shown("a")).is_err());
    }

    #[test]
    fn a_member_is_addressed_by_group_member_and_part() {
        let groups: Table = "outfit.a.body = {}\noutfit.a.hand = {}\npose.b.held = {}\n".parse().unwrap();
        assert_eq!(addresses(Some(&groups)), ["outfit/a/body", "outfit/a/hand", "pose/b/held"]);
    }

    #[test]
    fn what_was_chosen_holds_only_while_it_exists() {
        let members = ["a".to_string(), "b".to_string()];
        assert_eq!(member_of(&members, Some("b".to_string())).as_deref(), Some("b"));
        assert_eq!(member_of(&members, Some("gone".to_string())).as_deref(), Some("a"));
        assert_eq!(member_of(&members, None).as_deref(), Some("a"));
        assert_eq!(member_of(&[], None), None);
    }

    #[test]
    fn a_change_reaches_where_it_is_said_and_only_there() {
        let root = parsed(PREFAB).remove("groups").unwrap();
        let steps = ["outfit".to_string(), "b".to_string(), "body".to_string()];
        let new = changed(root.clone(), &steps, |fill| drop(fill.insert("translation".to_string(), Value::Integer(1))))
            .unwrap();
        assert!(new["outfit"]["b"]["body"].get("translation").is_some());
        assert!(new["outfit"]["a"]["body"].get("translation").is_none());
        assert!(changed(root, &["outfit".to_string(), "gone".to_string()], |_| ()).is_none());
    }

    #[test]
    fn what_hangs_from_what_is_followed_down() {
        let parts = fills("a");
        assert!(hangs_from(&parts, "held", "hand"));
        assert!(!hangs_from(&parts, "hand", "held"));
        assert!(hangs_from(&parts, "hand", "hand"), "a part cannot go onto itself");
    }
}
