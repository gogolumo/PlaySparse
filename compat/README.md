# Compatibility evidence

`profiles/*.json` are bounded, strict data records. They cannot execute commands or import plugins. `playsparse_desktop::compat::Record::parse` validates traversal, control characters, size/count limits and evidence provenance. Profiles select specifications; they never grant launch confirmation. Discovery sets **Detected**, not Compatible. Executable fingerprints bind confirmation to the source and mount bytes. A changed target requires unmount and reconfirmation.

States: Unknown, Detected, Tested, Compatible, CompatibleWithLimitations, Unsupported, Broken. Tested/Compatible/Broken require version, date and a receipt with a full commit SHA and an explicit evidence level. Evidence levels are CI simulation, hosted native test, physical hardware validation and commercial game compatibility. These are self-described evidence records, not cryptographically authenticated certifications. Synthetic receipts must not be reused as commercial evidence.

The native `playsparse-fixture` has plain, launcher, multiple descendants, child-first, case, save, argv and crash modes. It contains no commercial assets. Missing-executable is deliberately absent. On a case-insensitive source volume the distinct-case profile is Unsupported; do not claim case translation. Add profiles without changing React; add executable workload modes/tests in the Rust fixture when new behaviour is required. Profile targets are never automatically launched.

Portable process tests execute generated native fixtures using owned OS process handles. Mount validation uses the downloadable validation kit. A passing standalone fixture does not prove launching signed native code through a virtual filesystem, broker/DRM behaviour or commercial compatibility.
