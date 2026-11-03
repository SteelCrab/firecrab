use super::*;

#[test]
fn frontmatter_reads_name_description_and_license() {
    let meta = parse_frontmatter(
        "---\nname: pdf\ndescription: Use this skill whenever the user wants PDFs.\nlicense: Proprietary. LICENSE.txt has complete terms\n---\n# PDF\n",
    )
    .unwrap();
    assert_eq!(meta.name, "pdf");
    assert_eq!(
        meta.description,
        "Use this skill whenever the user wants PDFs."
    );
    assert_eq!(
        meta.license.as_deref(),
        Some("Proprietary. LICENSE.txt has complete terms")
    );
}

#[test]
fn frontmatter_without_license_is_none() {
    let meta =
        parse_frontmatter("---\nname: skill-creator\ndescription: Create skills.\n---\n").unwrap();
    assert_eq!(meta.license, None);
}

#[test]
fn frontmatter_strips_matching_quotes() {
    let meta =
        parse_frontmatter("---\nname: 'quoted'\ndescription: \"Quoted text\"\n---\n").unwrap();
    assert_eq!(meta.name, "quoted");
    assert_eq!(meta.description, "Quoted text");
}

#[test]
fn frontmatter_requires_fences_and_name() {
    assert!(parse_frontmatter("name: x\ndescription: y\n").is_err());
    assert!(parse_frontmatter("---\ndescription: y\n---\n").is_err());
    assert!(parse_frontmatter("---\nname: x\ndescription: y\n").is_err());
}

#[test]
fn classify_apache_from_license_txt() {
    assert_eq!(
        classify(
            Some("Complete terms in LICENSE.txt"),
            Some("\n   Apache License\n   Version 2.0, January 2004")
        ),
        License::Apache2
    );
    assert_eq!(
        classify(None, Some("Apache License\nVersion 2.0, January 2004")),
        License::Apache2
    );
}

#[test]
fn classify_proprietary_field_wins() {
    assert_eq!(
        classify(
            Some("Proprietary. LICENSE.txt has complete terms"),
            Some("Apache License\nVersion 2.0")
        ),
        License::Proprietary
    );
}

#[test]
fn classify_missing_everything_is_unknown() {
    assert_eq!(classify(None, None), License::Unknown);
    assert_eq!(classify(None, Some("MIT License")), License::Unknown);
    assert_eq!(
        classify(Some("Complete terms in LICENSE.txt"), None),
        License::Unknown
    );
}
