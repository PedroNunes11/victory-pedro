//! Compute programs of the title whose host pipelines are not made, by the
//! hash of their container (the first 8 bytes of its BLAKE3, as the logs
//! print it), and why ([`super::make`]).

/// Programs that shade what the console's ray-tracing structures were hit
/// by. They stand in the pipeline queue before the passes that make the
/// picture (all are bound at the world's first frame) and each takes the
/// AMD compiler 5 to 9 s (RX 7800 XT, one core each); with the structure
/// builder off (the default) there is no hit for them to shade. Sorted:
/// the 348 of the programs saved from earlier runs (`VICTORY_DUMP_SHADERS`);
/// more can be left out with `VICTORY_LEFT_OUT`.
#[rustfmt::skip]
pub(super) const RAY_TRACING_RESOLVES: [&str; 348] = [
    "008f3e09ac72b1e0", "015832d72477fb26", "02a4ed10c682ac3a", "02af016b81284952",
    "04d46e7934e1fe24", "04f90cb19f940759", "0512c381f84c2ae9", "05b939a0705f27ed",
    "05ef21a2132db12d", "06989306fb3b54c2", "072f5fe96efd27e1", "0753e7f1ebeb1681",
    "077869cd9b1456ee", "08f914eec26d44e7", "0926a680fc26dcba", "09a142d26deecbba",
    "0a12de46c4d5db08", "0ab92d33b2800cac", "0ae69044bfd15035", "0b07d50b76014236",
    "0b3d156291158496", "0bb64e3e583c4899", "0c0120fcb5aac198", "0cdb904653133be0",
    "0d0e7097e89fd890", "0ee452e79fc5b98c", "0efa55e3d4952b26", "0f1ea383d368b3c9",
    "11bb63dffff08dcc", "11d153b2f0b807ee", "11f60daa85075500", "1261271b20a35d1b",
    "12efa84143ac3f9c", "1351f0efaa452416", "137de2f8a7ef881c", "138715fe4ad3f11a",
    "149cb3508b989d0e", "1512b6b81c4072ed", "16622000e3a30bf0", "16e698c250d4e48e",
    "18406421ee8b6e8f", "19c48e8df2525284", "1a83504bcee28c3b", "1b20028fc5fae124",
    "1c02f8131c508c75", "1d495a6ae4665a95", "1e4947eada3d9ee3", "1f55c45de66dc2c4",
    "1f8e2bb1d26f9a53", "203f78efece5bbdf", "20ed134dfe212910", "22e10ac0ddb8bb73",
    "22f2cc5708df88be", "230256e20bfd61ab", "231d2daa66789312", "245c03778498b495",
    "254d39dc062ccbd2", "2582aab1c7e668e5", "264c19e1f1c9ae60", "2706619ccc47630b",
    "27ce027e7188bed3", "291be4e50b1272f1", "2964f72e06633f7c", "29be03ccaa4e353a",
    "2a5cb98cf0749a82", "2bb23624d292d127", "2bf91065944bcff4", "2c2cefe06f048390",
    "2c3c2602cf51d780", "2cc6decf9a73aedd", "2d3c50b5aff0f7fb", "2def80b5a6667abc",
    "2e28ea6fb9dad3c2", "2ec1dd27e95fdd16", "2f12d0f639fb7ed0", "31bc42e2ea163a1a",
    "326652d23fe73e99", "32738648a576fea0", "32eae53ef2498b71", "334cf8d4ab5e2968",
    "34121970d8a2e435", "355f182eb23a2ccf", "380028daa2706e89", "38a52b258e5d26bb",
    "3934246fb674f317", "39613a32023e4def", "3a59b63b97ad1abd", "3a6594961b9247e7",
    "3b47ec04e064477f", "3b741eb770b99f1f", "3b7b226a0af45f46", "3b9d6c2ddaae83d8",
    "3bc3bfd1cec35a7f", "3c5edc2725a39f30", "3d547602bdedb4eb", "3d7af3f375b862bd",
    "3e7e04c03727b40c", "3fa1fda35402c93f", "40769e6c239f937d", "4121071b5398393a",
    "422a5efe85d59e6a", "422ae0b850e3624e", "43a9fcb6c2636c61", "44dfb4c9f4f58355",
    "464f5ba0daa4d1bb", "46539424133f9b74", "470abab5c440c9c8", "476446edcac7d521",
    "479975ca9ff7e370", "482d518532477e6f", "49c9aea86d416b02", "4a55d212cc63c4ad",
    "4c573a76d2eeba04", "4c780a471589d843", "4ca60aafca490f58", "4e69dc8770bf27fc",
    "4f9e4161d703f789", "5002dc855d7c1bdd", "501f0865fdf45dfd", "503cf21ee3992b39",
    "50477cd6f2bd62e8", "5068b601735705a4", "50fe6dbe276e004f", "511b165e8c6f13f1",
    "515371262d6da9f5", "517535025e084ebd", "523dd9fe1a846c57", "52ab310c43b9f99c",
    "52b84c47ea411af2", "535f8a954d8ec638", "537522c620805a34", "55153ab821a66fce",
    "55858df1827ed9d0", "569d5818db7555f3", "56e546e08e2e0bf5", "571bf6d258f65ed0",
    "57a99301636eec5a", "58a085bb8210cccc", "592680ab38031b20", "5b47ab4f89743d00",
    "5c9d6316a51968c5", "5d061e6aed59c57d", "5d4ad8bda6371f79", "5e5b5c98afaa1753",
    "5e98acb32ee72e90", "5ececdeb8cd235c2", "5f8cf29b763648eb", "5f970e9e47594541",
    "61d7c3333b73892d", "631d7f5bc2692fd2", "63cb9879559651fa", "63dcd49361dd8ec4",
    "649850fad69f9228", "67c3552004aca7aa", "6a19b6c520023886", "6a20509b65f9a5f1",
    "6a608f2053e604b5", "6a7fcb54bf246310", "6c86597c4a6c9c5a", "6debf18bceee5f8d",
    "6ea325014572ed8d", "710a6eca5746445e", "71d5752d4d13974c", "7364921ed673344b",
    "738c1f4089f14c47", "74930ddd74e36464", "74f02289c9df5432", "7514cdc54e3c9502",
    "75cbdae3b2da61bf", "76cfc8733e521f35", "77af7adee63083f5", "781e845d3e7ddced",
    "79d3f9f9b164ec42", "7b956e1f6d56609f", "7cb338ecaeac0aa9", "7d3d547e81d1d118",
    "7e36d2a6d7c6a341", "7f05ddab771799a9", "7f6c3d8d319e6d29", "7fb216a0c6dee3a9",
    "811ec8279c97c1f6", "842de0c32bc8dcc5", "85c42c66489010c0", "871583f211e7dae3",
    "871f1215f671882c", "87479c005e2b4043", "88f6e5d9344943be", "896cd6b48e0b735e",
    "89cc3adb9b3b587e", "8a068d3c7bcccc01", "8a13d3da7f64b8d1", "8b566eecfb71cacc",
    "8b789540787ce9b8", "8c9910d177e922b6", "8d5d3f97369050bd", "8d7bf99583310094",
    "8e8696b916e3b141", "8f8dd4e691330c74", "92646708c3651a59", "9453e092e33f7f9c",
    "9506d52d41abfd92", "965b1fd9c61467ee", "96f043b62d77793b", "97c8165dc4f77bea",
    "9a6eb0b35e956927", "9aa51ebb679b4e36", "9aa61fc93f91d095", "9baa27bb65889d34",
    "9cfd9c092e1d5a97", "9d4962b03185c07b", "9dc1b86497754e73", "9e644939640669fc",
    "9eeba5c4a5cc1bff", "9f1e01df5796c330", "9fb1ebea2c1a32d4", "9fd538515af9fc60",
    "a0a074e2d868e618", "a1266cc7391ae0d6", "a17ec51447e9b9f1", "a2d880ad5dea1f45",
    "a3f168d1790bbff9", "a4169662a3549d17", "a47d0bb7c7e1e8ca", "a4df980a1c65fdbe",
    "a5399b214713f180", "a5dcdb5ab74beaeb", "a65398c11dbc32ca", "a73284b60aa303ed",
    "a7aef9e157ccc7fb", "a7d9b706b3465142", "a97b0de6386eca68", "aa107f09071811ca",
    "aa221f469520e809", "aac91f96d7555409", "ad17f5c4f7acac25", "ad1cf51b8a6c7096",
    "ad8767d2da8435b8", "ada19c97379f54bc", "adc76ddaa3a13d20", "af4285d1da44a96e",
    "b01e713adfd1b66a", "b084beb3ae6e27e3", "b0b6a2622d6e72f0", "b2433b5b9845e711",
    "b2ff2ef0d6c98dd7", "b40b3d8b444ada08", "b5075976aab8a28f", "b56cb5227f57e719",
    "b74ef432b1989b12", "b7a7fb5ba5934a89", "b7fc9aa161dbdff0", "b83683e48abab9bd",
    "ba0853e333e60933", "bb9c70158c1e22f8", "bd232dec3bec4319", "be87288ad49b521b",
    "bf1a5424e1b24d5b", "bff3758517ad9142", "c11581d638466192", "c18e21e30faad154",
    "c3cb00c582d26394", "c4023b5da8f2ce0d", "c41429a8461b2032", "c47848abe71e05bb",
    "c59bba942079b5d6", "c5eea4706e290002", "c60a8cc2683470c3", "c896e7b54e00286a",
    "c8a92fb86f31447f", "c985aaf507dd3152", "ccb2ef0f61223a15", "cdfefd6c8bb5dddd",
    "cf4ce294accdd428", "d0130073bb6f7a7d", "d03d2f08eac22af2", "d2c52fcd936985b4",
    "d3a9cedce74d91ce", "d43289c11944a7a1", "d4343b9a6e4f2717", "d46dfa0a902264ca",
    "d5131e7000dad14f", "d5c18b9fbcd08f36", "d5ef4f2bb16d9fa3", "d6dba265fd10bc2b",
    "d6e3a104264dd4c6", "d7792cca59ed128f", "d7ad7fcacbe52893", "d84645aebf8dca8f",
    "d84c0ca92452e526", "d9075dd50b1cca09", "db40744873920422", "dbba7e080e26ec20",
    "dbdad6ff83aadb1c", "dbdf9f9df42b2c69", "dd002690745ec22d", "dd18ab2b9c21b361",
    "de314b30ec34ee35", "de4e3af66d68d980", "df1c34cb6d9dbcfa", "df28ea82c24dc056",
    "df68734027b2d0c3", "dffafa6052629c85", "e05a2c1364dd1988", "e09b484b7400df95",
    "e0ba27015903a5f3", "e211ec7b25919742", "e2bbb611de6511d4", "e5742545a3591526",
    "e5929e833978d558", "e596f88f20c3ec8f", "e5d41b7e3006b249", "e62845424338161f",
    "e685f0080b144a9c", "e74c876bd581859b", "e79dd4bd900b4200", "e81277c8be37103b",
    "e82463dae16a79ee", "e90b8dcb8b4cfc6b", "eaeb10a22b9d6d7d", "eb47e845b2cb0eba",
    "ebd66fe3453f755d", "ec8170ec97875620", "ecdc3a46f8db7a5d", "ee636271b480f425",
    "efc770e7f2c27b03", "efdb8133aabc37fe", "f018c205b5199d39", "f0641885a8311319",
    "f09b1760a72a26af", "f0afa89ecfb6944e", "f0d5c40a8c448a86", "f1914c6c99d9577d",
    "f38d99b7a22477c9", "f3c00fa863debb3b", "f60ea2c9d69cac35", "f639f0fc07d7a39b",
    "f6da51317fcef92e", "f73607ad12ab905d", "f91ef42e5b381cb9", "f96d5cb8d72d9461",
    "fb666fb12f337286", "fc0cd9b89adde3ab", "fce7d2cbef0f4a24", "fd9778f48ea9e227",
    "fdfd1675ebbd57bc", "fe0c502f0ee290a7", "ff26243811447aed", "ffa23aec3187f880",
];

/// The program that finds the rays to trace for the screen's probes: 66,501
/// console instructions and 9,948 branches. Cleaned up (`emit::shader_opt`)
/// AMD's compiler takes 85 s and 1.3 GB, once; translated without the
/// clean-up passes it did not finish (47 GB after 8 minutes on the
/// RX 7800 XT). Left out only with `VICTORY_ACTIVATE_RAYS=0`.
pub(super) const ACTIVATE_RAYS: &str = "39f0e9aeb156a61c";

#[cfg(test)]
mod tests {
    #[test]
    fn the_resolves_are_sorted_hashes() {
        let all = super::RAY_TRACING_RESOLVES;
        assert!(
            all.windows(2).all(|w| w[0] < w[1]),
            "sorted and without repeats"
        );
        assert!(
            all.iter()
                .all(|h| h.len() == 16 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
        );
        assert!(all.binary_search(&super::ACTIVATE_RAYS).is_err());
    }
}
