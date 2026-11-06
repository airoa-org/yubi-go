fn main() {
    urdf_joints();

    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads");
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };
    let remote = git(&["remote", "get-url", "origin"]);
    let uri = match remote.strip_prefix("git@") {
        Some(rest) => format!("https://{}", rest.replacen(':', "/", 1)),
        None => without_userinfo(&remote),
    };
    println!("cargo:rustc-env=YUBIGO_GIT_URI={uri}");
    println!("cargo:rustc-env=YUBIGO_GIT_HASH={}", git(&["rev-parse", "HEAD"]));
    println!("cargo:rustc-env=YUBIGO_GIT_BRANCH={}", git(&["rev-parse", "--abbrev-ref", "HEAD"]));
}

/// `https://user:token@host/…` as `https://host/…`, because the URI is
/// recorded into every take's metadata.
fn without_userinfo(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else { return url.to_string() };
    let authority = rest.split('/').next().unwrap_or_default();
    match authority.rsplit_once('@') {
        Some((_, host)) => format!("{scheme}://{host}{}", &rest[authority.len()..]),
        None => url.to_string(),
    }
}

fn urdf_joints() {
    println!("cargo:rerun-if-changed=assets/yubi.urdf");
    let text = std::fs::read_to_string("assets/yubi.urdf").unwrap();
    let urdf = roxmltree::Document::parse(&text).unwrap();
    let mut rows = String::new();
    for joint in urdf.descendants().filter(|n| n.has_tag_name("joint")) {
        let attr = |tag, key| {
            joint.children().find(|n| n.has_tag_name(tag)).and_then(|n| n.attribute(key))
        };
        let nums = |tag, key, default| -> Vec<f64> {
            let text = attr(tag, key).unwrap_or(default);
            text.split_whitespace().map(|x| x.parse().unwrap()).collect()
        };
        let kind = joint.attribute("type").unwrap();
        let kind = kind[..1].to_uppercase() + &kind[1..];
        rows += &format!(
            "Joint {{ kind: JointKind::{kind}, parent: {:?}, child: {:?}, xyz: {:?}, rpy: {:?}, \
             axis: {:?}, limits: [{:?}, {:?}] }},\n",
            attr("parent", "link").unwrap(),
            attr("child", "link").unwrap(),
            nums("origin", "xyz", "0 0 0"),
            nums("origin", "rpy", "0 0 0"),
            nums("axis", "xyz", "1 0 0"),
            nums("limit", "lower", "0")[0],
            nums("limit", "upper", "0")[0],
        );
    }
    let out = std::env::var("OUT_DIR").unwrap();
    std::fs::write(format!("{out}/urdf_joints.rs"), format!("[\n{rows}]\n")).unwrap();
}
