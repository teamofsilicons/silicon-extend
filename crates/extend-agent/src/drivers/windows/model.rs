//! The element tree Extend reads on Windows, and how it becomes a snapshot.
//!
//! The Windows capture (`uia.rs`) produces a flat list of [`RawNode`]s in document order. Everything
//! from there on is pure: refs (`@e1`, `@e2`, …), the `-i` / `-d` / `-s` projections, and the text and
//! JSON shapes, which match the device engine's desktop snapshots so a Silicon reads a Windows screen the
//! same way it reads a Mac or Linux one.

use serde_json::{Value, json};

/// UI Automation control type ids (`UIA_*ControlTypeId`). `APPLICATION` is Extend's own synthetic
/// root for an app's windows; it isn't a UIA id.
pub mod ct {
    pub const APPLICATION: i32 = 0;
    pub const BUTTON: i32 = 50000;
    pub const CALENDAR: i32 = 50001;
    pub const CHECK_BOX: i32 = 50002;
    pub const COMBO_BOX: i32 = 50003;
    pub const EDIT: i32 = 50004;
    pub const HYPERLINK: i32 = 50005;
    pub const IMAGE: i32 = 50006;
    pub const LIST_ITEM: i32 = 50007;
    pub const LIST: i32 = 50008;
    pub const MENU: i32 = 50009;
    pub const MENU_BAR: i32 = 50010;
    pub const MENU_ITEM: i32 = 50011;
    pub const PROGRESS_BAR: i32 = 50012;
    pub const RADIO_BUTTON: i32 = 50013;
    pub const SCROLL_BAR: i32 = 50014;
    pub const SLIDER: i32 = 50015;
    pub const SPINNER: i32 = 50016;
    pub const STATUS_BAR: i32 = 50017;
    pub const TAB: i32 = 50018;
    pub const TAB_ITEM: i32 = 50019;
    pub const TEXT: i32 = 50020;
    pub const TOOL_BAR: i32 = 50021;
    pub const TOOL_TIP: i32 = 50022;
    pub const TREE: i32 = 50023;
    pub const TREE_ITEM: i32 = 50024;
    pub const CUSTOM: i32 = 50025;
    pub const GROUP: i32 = 50026;
    pub const THUMB: i32 = 50027;
    pub const DATA_GRID: i32 = 50028;
    pub const DATA_ITEM: i32 = 50029;
    pub const DOCUMENT: i32 = 50030;
    pub const SPLIT_BUTTON: i32 = 50031;
    pub const WINDOW: i32 = 50032;
    pub const PANE: i32 = 50033;
    pub const HEADER: i32 = 50034;
    pub const HEADER_ITEM: i32 = 50035;
    pub const TABLE: i32 = 50036;
    pub const TITLE_BAR: i32 = 50037;
    pub const SEPARATOR: i32 = 50038;
    pub const SEMANTIC_ZOOM: i32 = 50039;
    pub const APP_BAR: i32 = 50040;
}

/// Most nodes one snapshot reads before it stops and says `truncated`.
pub const MAX_NODES: usize = 1500;
/// Deepest the capture walks below a window.
pub const MAX_DEPTH: usize = 40;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn is_empty(&self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }
    pub fn center(&self) -> (i32, i32) {
        (
            (self.x + self.width / 2.0).round() as i32,
            (self.y + self.height / 2.0).round() as i32,
        )
    }
}

/// One element as UI Automation reported it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RawNode {
    /// Depth below the snapshot root (the synthetic application node is 0).
    pub depth: usize,
    /// Index of the parent in the same list.
    pub parent: Option<usize>,
    /// A `ct::*` id.
    pub control_type: i32,
    pub class_name: String,
    pub name: String,
    pub value: Option<String>,
    pub automation_id: String,
    pub rect: Option<Rect>,
    pub enabled: bool,
    pub focused: bool,
    pub keyboard_focusable: bool,
    pub offscreen: bool,
    pub selected: Option<bool>,
    pub checked: Option<bool>,
    /// Has a writable Value pattern.
    pub editable: bool,
    pub scrollable: bool,
    /// Has an Invoke, Toggle, SelectionItem or ExpandCollapse pattern.
    pub invokable: bool,
    pub pid: u32,
    pub app_name: String,
    pub app_id: String,
    pub window_title: String,
}

impl RawNode {
    /// The device-engine style type name (`Button`, `Edit`, …), used in JSON `type`.
    pub fn type_name(&self) -> &'static str {
        type_name(self.control_type)
    }

    /// The role Extend prints between brackets (`button`, `text-field`, …).
    pub fn role(&self) -> &'static str {
        if self.control_type == ct::PANE && self.scrollable {
            return "scroll-area";
        }
        if self.control_type == ct::DOCUMENT && !self.editable {
            return "text-view";
        }
        role_label(self.control_type)
    }

    /// On screen, with a size.
    pub fn visible(&self) -> bool {
        !self.offscreen && self.rect.is_some_and(|r| !r.is_empty())
    }

    /// Something a pointer can act on right now.
    pub fn hittable(&self) -> bool {
        self.visible() && self.enabled
    }

    /// Worth listing in an `-i` (interactive) snapshot.
    pub fn is_interactive(&self) -> bool {
        match self.control_type {
            ct::APPLICATION | ct::WINDOW => true,
            ct::BUTTON
            | ct::CHECK_BOX
            | ct::COMBO_BOX
            | ct::EDIT
            | ct::HYPERLINK
            | ct::LIST_ITEM
            | ct::MENU_ITEM
            | ct::RADIO_BUTTON
            | ct::SLIDER
            | ct::SPINNER
            | ct::TAB_ITEM
            | ct::TREE_ITEM
            | ct::SPLIT_BUTTON
            | ct::DATA_ITEM
            | ct::HEADER_ITEM
            | ct::DOCUMENT => self.visible() || self.control_type == ct::MENU_ITEM,
            _ => self.visible() && (self.invokable || self.editable || self.scrollable),
        }
    }

    fn is_editable_role(&self) -> bool {
        matches!(self.role(), "text-field" | "text-view" | "search")
    }

    /// What the device engine's `displayLabel` shows for this node.
    pub fn display_label(&self) -> String {
        let label = clean(&self.name);
        let value = self.value.as_deref().map(clean).unwrap_or_default();
        if self.is_editable_role() {
            if !value.is_empty() {
                return value;
            }
            if !label.is_empty() {
                return label;
            }
        } else if !label.is_empty() {
            return label;
        }
        if !value.is_empty() {
            return value;
        }
        clean(&self.automation_id)
    }

    /// The bracketed markers after the label.
    pub fn markers(&self) -> Vec<&'static str> {
        let mut m = Vec::new();
        if !self.enabled {
            m.push("disabled");
        }
        if self.selected == Some(true) {
            m.push("selected");
        }
        match self.checked {
            Some(true) => m.push("checked"),
            Some(false) => m.push("unchecked"),
            None => {}
        }
        if self.focused {
            m.push("focused");
        }
        if self.is_editable_role() && (self.editable || self.control_type == ct::EDIT) {
            m.push("editable");
        }
        if self.scrollable || self.role() == "scroll-area" {
            m.push("scrollable");
        }
        m
    }
}

/// One line of text, whitespace folded, control characters dropped.
fn clean(s: &str) -> String {
    let folded: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    folded.chars().filter(|c| !c.is_control()).collect()
}

pub fn type_name(control_type: i32) -> &'static str {
    match control_type {
        ct::APPLICATION => "Application",
        ct::BUTTON => "Button",
        ct::CALENDAR => "Calendar",
        ct::CHECK_BOX => "CheckBox",
        ct::COMBO_BOX => "ComboBox",
        ct::EDIT => "Edit",
        ct::HYPERLINK => "Hyperlink",
        ct::IMAGE => "Image",
        ct::LIST_ITEM => "ListItem",
        ct::LIST => "List",
        ct::MENU => "Menu",
        ct::MENU_BAR => "MenuBar",
        ct::MENU_ITEM => "MenuItem",
        ct::PROGRESS_BAR => "ProgressBar",
        ct::RADIO_BUTTON => "RadioButton",
        ct::SCROLL_BAR => "ScrollBar",
        ct::SLIDER => "Slider",
        ct::SPINNER => "Spinner",
        ct::STATUS_BAR => "StatusBar",
        ct::TAB => "Tab",
        ct::TAB_ITEM => "TabItem",
        ct::TEXT => "Text",
        ct::TOOL_BAR => "ToolBar",
        ct::TOOL_TIP => "ToolTip",
        ct::TREE => "Tree",
        ct::TREE_ITEM => "TreeItem",
        ct::CUSTOM => "Custom",
        ct::GROUP => "Group",
        ct::THUMB => "Thumb",
        ct::DATA_GRID => "DataGrid",
        ct::DATA_ITEM => "DataItem",
        ct::DOCUMENT => "Document",
        ct::SPLIT_BUTTON => "SplitButton",
        ct::WINDOW => "Window",
        ct::PANE => "Pane",
        ct::HEADER => "Header",
        ct::HEADER_ITEM => "HeaderItem",
        ct::TABLE => "Table",
        ct::TITLE_BAR => "TitleBar",
        ct::SEPARATOR => "Separator",
        ct::SEMANTIC_ZOOM => "SemanticZoom",
        ct::APP_BAR => "AppBar",
        _ => "Element",
    }
}

/// the device engine role names for UIA control types.
pub fn role_label(control_type: i32) -> &'static str {
    match control_type {
        ct::APPLICATION => "application",
        ct::BUTTON | ct::SPLIT_BUTTON => "button",
        ct::CALENDAR => "calendar",
        ct::CHECK_BOX => "checkbox",
        ct::COMBO_BOX => "combobox",
        ct::EDIT => "text-field",
        ct::HYPERLINK => "link",
        ct::IMAGE => "image",
        ct::LIST_ITEM => "cell",
        ct::LIST => "list",
        ct::MENU => "menu",
        ct::MENU_BAR => "menu-bar",
        ct::MENU_ITEM => "menu-item",
        ct::PROGRESS_BAR => "progress-indicator",
        ct::RADIO_BUTTON => "radio",
        ct::SCROLL_BAR => "scrollbar",
        ct::SLIDER => "slider",
        ct::SPINNER => "spinner",
        ct::STATUS_BAR => "status-bar",
        ct::TAB => "tab-group",
        ct::TAB_ITEM => "tab",
        ct::TEXT => "text",
        ct::TOOL_BAR | ct::APP_BAR => "toolbar",
        ct::TOOL_TIP => "tooltip",
        ct::TREE => "tree",
        ct::TREE_ITEM => "tree-item",
        ct::CUSTOM => "custom",
        ct::GROUP | ct::PANE | ct::SEMANTIC_ZOOM => "group",
        ct::THUMB => "thumb",
        ct::DATA_GRID | ct::TABLE => "table",
        ct::DATA_ITEM => "row",
        ct::DOCUMENT => "text-view",
        ct::WINDOW => "window",
        ct::HEADER => "header",
        ct::HEADER_ITEM => "header-item",
        ct::TITLE_BAR => "title-bar",
        ct::SEPARATOR => "separator",
        _ => "element",
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SnapshotOptions {
    /// `-i`: only what can be acted on, listed flat.
    pub interactive: bool,
    /// `-d <n>`: nodes at most this deep below the root.
    pub depth: Option<usize>,
    /// `-s <text>`: the subtree of the first node whose label, value or id contains this.
    pub scope: Option<String>,
    /// `--raw`: every node as one JSON line.
    pub raw: bool,
}

/// One node in a snapshot, pointing back at its [`RawNode`].
#[derive(Debug, Clone, PartialEq)]
pub struct SnapNode {
    pub raw_index: usize,
    /// `e1`, `e2`, … (without the `@`).
    pub reference: String,
    /// Depth relative to the snapshot root.
    pub depth: usize,
    /// Index of the parent in the snapshot's own node list, when it's in the snapshot.
    pub parent: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    pub nodes: Vec<SnapNode>,
    pub truncated: bool,
    pub app_name: String,
    pub app_id: String,
    pub options: SnapshotOptions,
}

/// Every index at or below `root` in document order (a subtree is contiguous in a DFS list).
fn subtree(raw: &[RawNode], root: usize) -> std::ops::Range<usize> {
    let base = raw[root].depth;
    let mut end = root + 1;
    while end < raw.len() && raw[end].depth > base {
        end += 1;
    }
    root..end
}

fn scope_matches(node: &RawNode, needle: &str) -> bool {
    let needle = needle.to_lowercase();
    node.name.to_lowercase().contains(&needle)
        || node
            .value
            .as_deref()
            .is_some_and(|v| v.to_lowercase().contains(&needle))
        || node.automation_id.to_lowercase().contains(&needle)
}

/// Builds a snapshot from a capture. Refs are numbered in document order over the kept nodes.
pub fn build_snapshot(
    raw: &[RawNode],
    options: &SnapshotOptions,
    app_name: &str,
    app_id: &str,
    truncated: bool,
) -> Snapshot {
    let project = |range: std::ops::Range<usize>| -> Vec<usize> {
        let base = raw.get(range.start).map_or(0, |n| n.depth);
        range
            .filter(|&i| options.depth.is_none_or(|d| raw[i].depth - base <= d))
            .filter(|&i| !options.interactive || raw[i].is_interactive())
            .collect()
    };
    let (kept, base) = match options.scope.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(needle) => {
            let found = (0..raw.len())
                .filter(|&i| scope_matches(&raw[i], needle.trim()))
                .map(|i| project(subtree(raw, i)))
                .find(|kept| !kept.is_empty());
            match found {
                Some(kept) => {
                    let base = raw[kept[0]]
                        .depth
                        .min(kept.iter().map(|&i| raw[i].depth).min().unwrap_or(0));
                    (kept, base)
                }
                None => (vec![], 0),
            }
        }
        None => (project(0..raw.len()), 0),
    };
    let mut position = std::collections::HashMap::new();
    let mut nodes = Vec::with_capacity(kept.len());
    for (n, &i) in kept.iter().enumerate() {
        position.insert(i, n);
        let mut parent = raw[i].parent;
        let mut snap_parent = None;
        while let Some(p) = parent {
            if let Some(&sp) = position.get(&p) {
                snap_parent = Some(sp);
                break;
            }
            parent = raw[p].parent;
        }
        nodes.push(SnapNode {
            raw_index: i,
            reference: format!("e{}", n + 1),
            depth: if options.interactive {
                0
            } else {
                raw[i].depth.saturating_sub(base)
            },
            parent: snap_parent,
        });
    }
    Snapshot {
        nodes,
        truncated,
        app_name: app_name.to_owned(),
        app_id: app_id.to_owned(),
        options: options.clone(),
    }
}

impl Snapshot {
    /// The raw index a ref (`@e3` or `e3`) points at.
    pub fn resolve_ref(&self, reference: &str) -> Option<usize> {
        let r = reference.strip_prefix('@').unwrap_or(reference);
        // the device engine pins refs as `@e12~s4`; the pin doesn't matter within one snapshot.
        let r = r.split('~').next().unwrap_or(r);
        self.nodes.iter().find(|n| n.reference == r).map(|n| n.raw_index)
    }

    /// The ref of a raw node, if it's in this snapshot.
    pub fn ref_of(&self, raw_index: usize) -> Option<&str> {
        self.nodes
            .iter()
            .find(|n| n.raw_index == raw_index)
            .map(|n| n.reference.as_str())
    }

    /// The text the device engine prints for a snapshot.
    pub fn to_text(&self, raw: &[RawNode]) -> String {
        let mut out = String::new();
        if !self.app_name.is_empty() {
            out.push_str(&format!("Page: {}\n", self.app_name));
        }
        if !self.app_id.is_empty() {
            out.push_str(&format!("App: {}\n", self.app_id));
        }
        out.push_str(&format!(
            "Snapshot: {} nodes{}\n",
            self.nodes.len(),
            if self.truncated { " (truncated)" } else { "" }
        ));
        if self.options.raw {
            for (n, node) in self.nodes.iter().enumerate() {
                out.push_str(&self.node_json(raw, n, node).to_string());
                out.push('\n');
            }
            return out;
        }
        // Unlabelled groups are structure, not content: skipped, and their children move up.
        let mut visible_depths: Vec<usize> = Vec::new();
        for node in &self.nodes {
            let r = &raw[node.raw_index];
            let label = r.display_label();
            if r.role() == "group" && label.is_empty() {
                continue;
            }
            while visible_depths.last().is_some_and(|&d| node.depth <= d) {
                visible_depths.pop();
            }
            let depth = visible_depths.len();
            visible_depths.push(node.depth);
            let mut line = format!("{}@{} [{}]", "  ".repeat(depth), node.reference, r.role());
            if !label.is_empty() {
                line.push_str(&format!(" \"{label}\""));
            }
            for m in r.markers() {
                line.push_str(&format!(" [{m}]"));
            }
            out.push_str(line.trim_end());
            out.push('\n');
        }
        out
    }

    fn node_json(&self, raw: &[RawNode], index: usize, node: &SnapNode) -> Value {
        node_json(&raw[node.raw_index], &node.reference, index, node.depth, node.parent)
    }

    /// The JSON the device engine returns for `snapshot --json`.
    pub fn to_json(&self, raw: &[RawNode]) -> Value {
        let nodes: Vec<Value> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| self.node_json(raw, i, n))
            .collect();
        json!({
            "nodes": nodes,
            "truncated": self.truncated,
            "appName": self.app_name,
            "appBundleId": self.app_id,
            "platform": "windows",
        })
    }
}

/// One node in the device engine's JSON shape.
pub fn node_json(r: &RawNode, reference: &str, index: usize, depth: usize, parent: Option<usize>) -> Value {
    let mut v = json!({
        "ref": reference,
        "index": index,
        "depth": depth,
        "type": r.type_name(),
        "role": r.role(),
        "enabled": r.enabled,
        "hittable": r.hittable(),
        "pid": r.pid,
    });
    let m = v.as_object_mut().expect("object");
    if let Some(p) = parent {
        m.insert("parentIndex".into(), json!(p));
    }
    let label = clean(&r.name);
    if !label.is_empty() {
        m.insert("label".into(), json!(label));
    }
    if let Some(value) = r.value.as_deref().filter(|v| !v.is_empty()) {
        m.insert("value".into(), json!(value));
    }
    if !r.automation_id.is_empty() {
        m.insert("identifier".into(), json!(r.automation_id));
    }
    if let Some(rect) = r.rect {
        m.insert(
            "rect".into(),
            json!({"x": rect.x, "y": rect.y, "width": rect.width, "height": rect.height}),
        );
    }
    if r.focused {
        m.insert("focused".into(), json!(true));
    }
    if let Some(s) = r.selected {
        m.insert("selected".into(), json!(s));
    }
    if let Some(c) = r.checked {
        m.insert("checked".into(), json!(c));
    }
    if !r.app_name.is_empty() {
        m.insert("appName".into(), json!(r.app_name));
    }
    if !r.app_id.is_empty() {
        m.insert("bundleId".into(), json!(r.app_id));
    }
    if !r.window_title.is_empty() {
        m.insert("windowTitle".into(), json!(r.window_title));
    }
    if !r.class_name.is_empty() {
        m.insert("className".into(), json!(r.class_name));
    }
    v
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn node(depth: usize, parent: Option<usize>, control_type: i32, name: &str) -> RawNode {
        RawNode {
            depth,
            parent,
            control_type,
            name: name.into(),
            rect: Some(Rect {
                x: 10.0,
                y: 20.0,
                width: 100.0,
                height: 30.0,
            }),
            enabled: true,
            pid: 42,
            app_name: "Notepad".into(),
            app_id: "notepad.exe".into(),
            window_title: "Untitled - Notepad".into(),
            ..Default::default()
        }
    }

    /// A small Notepad-like tree.
    pub fn notepad() -> Vec<RawNode> {
        let mut v = vec![
            node(0, None, ct::APPLICATION, "Notepad"),
            node(1, Some(0), ct::WINDOW, "Untitled - Notepad"),
            node(2, Some(1), ct::PANE, ""),
            node(3, Some(2), ct::DOCUMENT, "Text editor"),
            node(2, Some(1), ct::MENU_BAR, "Application"),
            node(3, Some(4), ct::MENU_ITEM, "File"),
            node(3, Some(4), ct::MENU_ITEM, "Edit"),
            node(2, Some(1), ct::BUTTON, "Close"),
            node(2, Some(1), ct::CHECK_BOX, "Word wrap"),
            node(2, Some(1), ct::TEXT, "Ln 1, Col 1"),
        ];
        v[3].editable = true;
        v[3].value = Some("hello\nworld".into());
        v[3].focused = true;
        v[3].keyboard_focusable = true;
        v[7].invokable = true;
        v[8].checked = Some(false);
        v[8].enabled = false;
        v
    }

    #[test]
    fn full_snapshot_text_matches_agent_device_shape() {
        let raw = notepad();
        let snap = build_snapshot(&raw, &SnapshotOptions::default(), "Notepad", "notepad.exe", false);
        assert_eq!(snap.nodes.len(), 10);
        let text = snap.to_text(&raw);
        let expected = concat!(
            "Page: Notepad\n",
            "App: notepad.exe\n",
            "Snapshot: 10 nodes\n",
            "@e1 [application] \"Notepad\"\n",
            "  @e2 [window] \"Untitled - Notepad\"\n",
            "    @e4 [text-view] \"hello world\" [focused] [editable]\n",
            "    @e5 [menu-bar] \"Application\"\n",
            "      @e6 [menu-item] \"File\"\n",
            "      @e7 [menu-item] \"Edit\"\n",
            "    @e8 [button] \"Close\"\n",
            "    @e9 [checkbox] \"Word wrap\" [disabled] [unchecked]\n",
            "    @e10 [text] \"Ln 1, Col 1\"\n",
        );
        assert_eq!(text, expected);
    }

    #[test]
    fn interactive_snapshot_is_flat_and_renumbered() {
        let raw = notepad();
        let opts = SnapshotOptions {
            interactive: true,
            ..Default::default()
        };
        let snap = build_snapshot(&raw, &opts, "Notepad", "notepad.exe", false);
        let text = snap.to_text(&raw);
        assert!(text.contains("Snapshot: 7 nodes"), "{text}");
        assert!(text.contains("\n@e3 [text-view] \"hello world\""), "{text}");
        assert!(text.contains("\n@e6 [button] \"Close\""), "{text}");
        assert!(!text.contains("Ln 1"), "static text is not interactive: {text}");
        assert!(!text.contains("  @"), "flat: {text}");
        assert_eq!(snap.resolve_ref("@e6"), Some(7));
        assert_eq!(snap.resolve_ref("e3"), Some(3));
        assert_eq!(snap.resolve_ref("@e6~s2"), Some(7));
        assert_eq!(snap.resolve_ref("@e99"), None);
        assert_eq!(snap.ref_of(7), Some("e6"));
    }

    #[test]
    fn depth_limits_and_scope_reroot() {
        let raw = notepad();
        let snap = build_snapshot(
            &raw,
            &SnapshotOptions {
                depth: Some(1),
                ..Default::default()
            },
            "",
            "",
            false,
        );
        assert_eq!(snap.nodes.len(), 2);
        let text = snap.to_text(&raw);
        assert_eq!(
            text,
            "Snapshot: 2 nodes\n@e1 [application] \"Notepad\"\n  @e2 [window] \"Untitled - Notepad\"\n"
        );

        let snap = build_snapshot(
            &raw,
            &SnapshotOptions {
                scope: Some("application".into()),
                ..Default::default()
            },
            "",
            "",
            false,
        );
        let text = snap.to_text(&raw);
        assert_eq!(
            text,
            "Snapshot: 3 nodes\n@e1 [menu-bar] \"Application\"\n  @e2 [menu-item] \"File\"\n  @e3 [menu-item] \"Edit\"\n"
        );

        let snap = build_snapshot(
            &raw,
            &SnapshotOptions {
                scope: Some("nothing here".into()),
                ..Default::default()
            },
            "",
            "",
            false,
        );
        assert!(snap.nodes.is_empty());
        assert_eq!(snap.to_text(&raw), "Snapshot: 0 nodes\n");
    }

    #[test]
    fn scope_skips_matches_with_empty_projection() {
        let raw = notepad();
        // Only the disabled "Word wrap" checkbox matches, and it is interactive, so it is the whole snapshot.
        let opts = SnapshotOptions {
            scope: Some("word".into()),
            interactive: true,
            ..Default::default()
        };
        let snap = build_snapshot(&raw, &opts, "", "", false);
        assert_eq!(snap.nodes.len(), 1);
        assert_eq!(snap.nodes[0].raw_index, 8);
    }

    #[test]
    fn truncated_and_raw_output() {
        let raw = notepad();
        let snap = build_snapshot(
            &raw,
            &SnapshotOptions {
                raw: true,
                depth: Some(1),
                ..Default::default()
            },
            "Notepad",
            "",
            true,
        );
        let text = snap.to_text(&raw);
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some("Page: Notepad"));
        assert_eq!(lines.next(), Some("Snapshot: 2 nodes (truncated)"));
        let first: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(first["ref"], "e1");
        assert_eq!(first["type"], "Application");
        let second: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(second["parentIndex"], 0);
        assert_eq!(second["windowTitle"], "Untitled - Notepad");
    }

    #[test]
    fn json_shape() {
        let raw = notepad();
        let snap = build_snapshot(&raw, &SnapshotOptions::default(), "Notepad", "notepad.exe", false);
        let j = snap.to_json(&raw);
        assert_eq!(j["appName"], "Notepad");
        assert_eq!(j["truncated"], false);
        let doc = &j["nodes"][3];
        assert_eq!(doc["ref"], "e4");
        assert_eq!(doc["role"], "text-view");
        assert_eq!(doc["value"], "hello\nworld");
        assert_eq!(doc["focused"], true);
        assert_eq!(doc["rect"]["width"], 100.0);
        assert_eq!(doc["parentIndex"], 2);
        assert_eq!(j["nodes"][8]["checked"], false);
        assert_eq!(j["nodes"][8]["enabled"], false);
    }

    #[test]
    fn labels_follow_display_rules() {
        let mut n = node(0, None, ct::EDIT, "Search");
        n.editable = true;
        assert_eq!(n.display_label(), "Search");
        n.value = Some("cats".into());
        assert_eq!(n.display_label(), "cats", "editable roles prefer the value");
        assert_eq!(n.markers(), vec!["editable"]);
        let mut b = node(0, None, ct::BUTTON, "");
        b.automation_id = "okButton".into();
        assert_eq!(b.display_label(), "okButton");
        let mut p = node(0, None, ct::PANE, "");
        p.scrollable = true;
        assert_eq!(p.role(), "scroll-area");
        assert_eq!(p.markers(), vec!["scrollable"]);
        let mut hidden = node(0, None, ct::BUTTON, "x");
        hidden.offscreen = true;
        assert!(!hidden.visible());
        assert!(!hidden.is_interactive());
        assert_eq!(role_label(ct::SPLIT_BUTTON), "button");
        assert_eq!(role_label(ct::LIST_ITEM), "cell");
        assert_eq!(role_label(12345), "element");
        assert_eq!(type_name(ct::DATA_GRID), "DataGrid");
    }

    #[test]
    fn rect_center() {
        assert_eq!(
            Rect {
                x: 10.0,
                y: 20.0,
                width: 100.0,
                height: 30.0
            }
            .center(),
            (60, 35)
        );
    }
}
