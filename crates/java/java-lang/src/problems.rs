//! Short titles for the problem codes of the server, as IDEA groups its inspections.
//!
//! The codes are the `IProblem` ids of Eclipse JDT, which jdtls puts into a diagnostic
//! as a number. Several ids stand for one kind of problem and share a title.

use lang_api::ProblemKind;

/// The common ids of `IProblem` and the title of the group each one belongs to.
const NAMES: &[(&str, &str)] = &[
    // Type mismatch and the members that promise another type.
    ("16777231", "Incompatible types"),
    ("16777232", "Incompatible types"),
    ("16777233", "Incompatible types"),
    ("16777235", "Incompatible types"),
    ("16777766", "Incompatible types"),
    ("16777769", "Incompatible types"),
    ("16777796", "Incompatible types"),
    ("553648793", "Incompatible types"),
    ("67109268", "Incompatible types"),
    ("67109277", "Incompatible types"),
    ("67109283", "Incompatible types"),
    ("603979902", "Incompatible types"),
    ("603979904", "Incompatible types"),
    ("2099066", "Incompatible types"),
    ("2099253", "Incompatible types"),
    // Names the compiler cannot resolve.
    ("16777218", "Cannot resolve type"),
    ("268435846", "Cannot resolve type"),
    ("67108984", "Cannot resolve type"),
    ("67109135", "Cannot resolve type"),
    ("134217857", "Cannot resolve type"),
    ("536871450", "Cannot resolve type"),
    ("8389908", "Cannot resolve type"),
    ("8389927", "Cannot resolve type"),
    ("67108964", "Cannot resolve method"),
    ("134217858", "Cannot resolve method"),
    ("134217868", "Cannot resolve method"),
    ("134217871", "Cannot resolve method"),
    ("33554502", "Cannot resolve symbol"),
    ("570425394", "Cannot resolve symbol"),
    ("67109475", "Cannot resolve symbol"),
    // Names the compiler finds but cannot use from here.
    ("16777219", "Not visible"),
    ("16777552", "Not visible"),
    ("33554503", "Not visible"),
    ("33554783", "Not visible"),
    ("268435847", "Not visible"),
    ("67108965", "Not visible"),
    ("67109235", "Not visible"),
    ("67109240", "Not visible"),
    ("67109245", "Not visible"),
    ("67109671", "Not visible"),
    ("134217859", "Not visible"),
    ("134217869", "Not visible"),
    ("134217872", "Not visible"),
    ("134218536", "Not visible"),
    // A type named twice in the same list.
    ("16777547", "Redundant superinterface"),
    // Deprecated members, including the terminally and since-version forms.
    ("16777221", "Deprecated member"),
    ("16778616", "Deprecated member"),
    ("16778621", "Deprecated member"),
    ("16778626", "Deprecated member"),
    ("16778649", "Deprecated member"),
    ("33554505", "Deprecated member"),
    ("33555835", "Deprecated member"),
    ("33555840", "Deprecated member"),
    ("33555845", "Deprecated member"),
    ("67108967", "Deprecated member"),
    ("67109276", "Deprecated member"),
    ("67110265", "Deprecated member"),
    ("67110266", "Deprecated member"),
    ("67110268", "Deprecated member"),
    ("67110270", "Deprecated member"),
    ("67110271", "Deprecated member"),
    ("67110273", "Deprecated member"),
    ("67110275", "Deprecated member"),
    ("67110276", "Deprecated member"),
    ("67110278", "Deprecated member"),
    ("134217861", "Deprecated member"),
    ("8390033", "Deprecated member"),
    ("8390034", "Deprecated member"),
    ("8390035", "Deprecated member"),
    ("8390036", "Deprecated member"),
    ("8390037", "Deprecated member"),
    ("8390038", "Deprecated member"),
    ("8390039", "Deprecated member"),
    ("8390040", "Deprecated member"),
    // Generic code that the compiler cannot check without a warning.
    ("16777428", "Unchecked or unsafe operation"),
    ("16777740", "Unchecked or unsafe operation"),
    ("16777746", "Unchecked or unsafe operation"),
    ("16777747", "Unchecked or unsafe operation"),
    ("16777748", "Unchecked or unsafe operation"),
    ("16777752", "Unchecked or unsafe operation"),
    ("16777761", "Unchecked or unsafe operation"),
    ("16777764", "Unchecked or unsafe operation"),
    ("16777767", "Unchecked or unsafe operation"),
    ("16777770", "Unchecked or unsafe operation"),
    ("16777771", "Unchecked or unsafe operation"),
    ("16777785", "Unchecked or unsafe operation"),
    ("16777786", "Unchecked or unsafe operation"),
    ("16777801", "Unchecked or unsafe operation"),
    ("67109423", "Unchecked or unsafe operation"),
    ("67109438", "Unchecked or unsafe operation"),
    // A parameterized type used without its arguments.
    ("16777777", "Raw use of parameterized class"),
    ("16777788", "Raw use of parameterized class"),
    ("2099067", "Raw use of parameterized class"),
    // Declarations nothing reads.
    ("268435844", "Unused import"),
    ("536870973", "Unused variable"),
    ("536870974", "Unused parameter"),
    ("536870997", "Unused parameter"),
    ("536871193", "Unused parameter"),
    ("553648135", "Unused private member"),
    ("570425421", "Unused private member"),
    ("603979894", "Unused private member"),
    ("603979910", "Unused private member"),
    ("16777877", "Unused declaration"),
    ("536871060", "Unused declaration"),
    ("536871097", "Unused declaration"),
    ("536871098", "Unused declaration"),
    ("67109443", "Unused declaration"),
    ("67109524", "Unused declaration"),
    ("536871111", "Unused label"),
    ("536871547", "Redundant suppression"),
    // Code no execution reaches.
    ("536871061", "Unreachable code"),
    ("536871073", "Unreachable code"),
    ("83886247", "Unreachable code"),
    // A method that must return a value and does not.
    ("603979884", "Missing return value"),
    ("67109635", "Missing return value"),
    ("16777327", "Missing return type"),
    // A local or a field read before it is assigned.
    ("978", "Variable might not have been initialized"),
    ("33554513", "Variable might not have been initialized"),
    ("33555202", "Variable might not have been initialized"),
    ("33555366", "Variable might not have been initialized"),
    ("33555367", "Variable might not have been initialized"),
    ("536870963", "Variable might not have been initialized"),
    ("536871681", "Variable might not have been initialized"),
    // The null analysis of the compiler.
    ("536871309", "Nullability problem"),
    ("536871310", "Nullability problem"),
    ("536871311", "Nullability problem"),
    ("536871363", "Nullability problem"),
    ("536871364", "Nullability problem"),
    ("536871365", "Nullability problem"),
    ("536871366", "Nullability problem"),
    ("536871367", "Nullability problem"),
    ("536871368", "Nullability problem"),
    ("536871369", "Nullability problem"),
    ("536871370", "Nullability problem"),
    ("536871371", "Nullability problem"),
    ("536871373", "Nullability problem"),
    ("536871582", "Nullability problem"),
    ("536871583", "Nullability problem"),
    ("536871584", "Nullability problem"),
    ("536871585", "Nullability problem"),
    ("536871831", "Nullability problem"),
    ("536871832", "Nullability problem"),
    ("536871853", "Nullability problem"),
    ("536871856", "Nullability problem"),
    ("536871857", "Nullability problem"),
    ("536871863", "Nullability problem"),
    ("536871864", "Nullability problem"),
    ("536871879", "Nullability problem"),
    ("16778126", "Nullability problem"),
    ("16778127", "Nullability problem"),
    ("16778128", "Nullability problem"),
    // Annotations that a declaration should carry.
    ("67109491", "Missing @Override"),
    ("67109500", "Missing @Override"),
    ("536871540", "Missing @Deprecated"),
    ("536871541", "Missing @Deprecated"),
    ("536871542", "Missing @Deprecated"),
    // A switch that does not handle every value.
    ("33555193", "Missing switch case"),
    ("33555200", "Missing switch case"),
    ("536871678", "Missing switch case"),
    ("536871679", "Missing switch case"),
    ("536872519", "Missing switch case"),
    ("536872522", "Missing switch case"),
    ("536872523", "Missing switch case"),
    // A concrete type that leaves an abstract method without a body.
    ("16777549", "Abstract method not implemented"),
    ("67109264", "Abstract method not implemented"),
    ("67109282", "Abstract method not implemented"),
    ("67109622", "Abstract method not implemented"),
    ("67109627", "Abstract method not implemented"),
    // Syntax that the parser reports with a code of its own.
    ("1610612960", "Missing semicolon"),
    ("536871092", "Superfluous semicolon"),
    // An information of the server rather than a problem of the code.
    ("1102", "Not analysed"),
];

/// The title of the group a problem code belongs to.
pub fn name(code: &str) -> Option<&'static str> {
    NAMES
        .iter()
        .find(|(known, _)| *known == code)
        .map(|(_, name)| *name)
}

/// Every known kind once, with the first code of its group.
pub fn kinds() -> Vec<ProblemKind> {
    let mut known: Vec<ProblemKind> = Vec::new();
    for (code, name) in NAMES {
        if known.iter().any(|kind| kind.name == *name) {
            continue;
        }
        known.push(ProblemKind {
            code: (*code).to_owned(),
            name: (*name).to_owned(),
        });
    }
    known
}

/// The first line of a message with the identifiers in quotes replaced by `…`, so
/// that findings of an unknown code still fall into one group.
pub fn message_template(message: &str) -> String {
    let mut text = String::new();
    let mut rest = message.lines().next().unwrap_or("");
    while let Some(at) = rest.find(['\'', '"', '`', '«']) {
        let quoted = &rest[at..];
        let mut characters = quoted.chars();
        let open = characters.next().unwrap_or_default();
        let close = if open == '«' { '»' } else { open };
        let Some(end) = quoted[open.len_utf8()..].find(close) else {
            break;
        };
        text.push_str(&rest[..at]);
        text.push('…');
        rest = &quoted[open.len_utf8() + end + close.len_utf8()..];
    }
    text.push_str(rest);
    text
}

#[cfg(test)]
mod tests {
    use super::{kinds, message_template, name};

    #[test]
    fn a_common_problem_code_has_a_short_title() {
        assert_eq!(name("16777233"), Some("Incompatible types"));
        assert_eq!(name("268435844"), Some("Unused import"));
        assert_eq!(name("536871061"), Some("Unreachable code"));
        assert_eq!(name("16777233"), name("16777235"));
        assert_eq!(name("no-such-code"), None);
    }

    #[test]
    fn every_kind_is_listed_once() {
        let kinds = kinds();
        assert!(kinds.len() > 10);
        let mut names: Vec<&str> = kinds.iter().map(|kind| kind.name.as_str()).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count);
    }

    #[test]
    fn an_unknown_code_keeps_a_message_without_its_identifiers() {
        assert_eq!(
            message_template("The method 'getPet' is undefined for the type 'Owner'"),
            "The method … is undefined for the type …"
        );
        assert_eq!(
            message_template("Type mismatch: cannot convert from String to int\nsecond line"),
            "Type mismatch: cannot convert from String to int"
        );
        assert_eq!(
            message_template("Cannot find «Foo» and `bar`"),
            "Cannot find … and …"
        );
    }
}
