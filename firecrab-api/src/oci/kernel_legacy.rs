//! Digests of kernels already installed by older releases.
//! These entries are never downloaded; new installations build upstream source.

use crate::image_install::Architecture;

pub(crate) fn image_digest(architecture: Architecture, version: &str) -> Option<&'static str> {
    let pins: &[(&str, &str)] = match architecture {
        Architecture::X86_64 => &[
            (
                "7.2.2-vsock1",
                "sha256:540c7de42c0ce8ddb46c58759fff3d83445c4cfe9d38e70e56c3b0cc2001a141",
            ),
            (
                "7.2.2",
                "sha256:351b23784e5e53de5af970ff6af7a074a916a55327ff802645f3583b39f3a4f1",
            ),
            (
                "7.1.12",
                "sha256:004c3031dfcc2deec974b55714a2ed1052903662531e8df2a3cb3380bda5674b",
            ),
            (
                "7.1.9",
                "sha256:079d2149a9378f5705da46232e99886187fecdd5517428d2c294b6bd1e0dca6b",
            ),
            (
                "7.1.8",
                "sha256:1d693ebb340ee418127aacf679080671679e455302a8f89ff8d4a45b6d293cdb",
            ),
        ],
        Architecture::Aarch64 => &[
            (
                "7.2.2-vsock1",
                "sha256:a69c57bde32d811e311f35bf0d7d42e5188be1ed21b2150140796a92385ed863",
            ),
            (
                "7.2.2",
                "sha256:f3acb729ded8213e4b157fd80746f6034cc4994888f73b82b1e34cea5813dbc0",
            ),
            (
                "7.1.12",
                "sha256:6d6205c7dccbadcd74fa0bf84082af2f13bd8492d7a489ee0e2a6891c00c10bd",
            ),
            (
                "7.1.9",
                "sha256:6d60d06429aa5e244cc5a0eb60383a97fde61f4aa40453d5cc6fecb59a64b58f",
            ),
            (
                "7.1.8",
                "sha256:f1ded792a87f2b2d798566d22485fe73367dc8f348670827638f1a6b43375a44",
            ),
        ],
    };
    pins.iter()
        .find(|(release, _)| *release == version)
        .map(|(_, digest)| *digest)
}
