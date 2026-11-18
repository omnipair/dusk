// One-off public account scope from the reconciled layout-17 inventory.
// Amounts and evidence are committed by the reviewed begin instruction.
use anchor_lang::prelude::*;
pub const MARKET: Pubkey = pubkey!("7xf68VEvMtLJ2Up5zVZdA161uH1yNkTFYgDFhNUSwkM7");
pub const AUTHORITY: Pubkey = pubkey!("EdYBBhu2PU84eNt6r8zPWkaxrxbXVAzVvAz3tR3XveRq");
pub const PROGRAM_DATA: Pubkey = pubkey!("58HpCNM5jydkig5bhy8BR77DJn2E3KcuD9hruj13mUqz");
pub const TREASURY: Pubkey = pubkey!("8s6Jdoh7tgUqmU3D2EmpNJHSvuN5U4NybpLAdsiMitwB");
pub const OWNERS: [Pubkey; 5] = [
    pubkey!("2K9ZpC3LVqRfR8Vveo92LhiofbDcF6PuDRJbaPp9V34m"),
    pubkey!("3XAr6KPC1x75XcBLVRc5TxD39gDXMGu4P1kFgDn5PFu2"),
    pubkey!("41shojZU4PwxKBV1MmX3yYyAHiGrF3DyDLAdvsuSCUm7"),
    pubkey!("5YE9xb49trAeZHzEcuusySswFb5pWdsZLCTYqiymoiFa"),
    pubkey!("8tF4uYMBXqGhCUGRZL3AmPqRzbX8JJ1TpYnY3uJKN4kt"),
];
pub const MINTS: [Pubkey; 2] = [
    pubkey!("omfgRBnxHsNJh6YeGbGAmWenNkenzsXyBXm3WDhmeta"),
    pubkey!("METAwkXcqyXKy1AtsSgJ8JiUHwGCafnZL38n3vYmeta"),
];
pub const PARAMS_HASH: [u8; 32] = [
    212, 49, 62, 24, 153, 22, 6, 68, 244, 178, 147, 74, 248, 9, 89, 146, 248, 162, 93, 205, 196, 35, 69, 83, 5, 243,
    133, 40, 0, 99, 8, 27,
];
pub const MARKET_BUMP: u8 = 252;
pub const VAULTS: [Pubkey; 8] = [
    pubkey!("GkWaRKsBgb4e7TrXv7Di1oyU8gkFzf5PNeBgW6oFQcsC"),
    pubkey!("BbdRzkSmjmsS3XTedqgM8RqFEmR4V1NvxGuECrwb8quw"),
    pubkey!("DwmcRX1J5oaFc9ftcbU3p1ftqEJTdRWc3RP7NjtTSR3"),
    pubkey!("8seUCsEbJiY2znwrBvSPRDCxtNjVr1Mo3BavmCRxVcHz"),
    pubkey!("Gk3sqyHHmZqF4dHg3HZDv8NVAmRWEnqVenE7cCe4Mbc2"),
    pubkey!("EA9HJBb32ZoukJE1yWdHo9SLTDFkYJGSW183T96S7FpR"),
    pubkey!("CZCennGC9gic3CnofzR6ME5k3mNmtutgaVaFSHkmPYPL"),
    pubkey!("8UukS7MkytKtqZ6sLh4F87KU2Z2wzbKYj2NmLW43Ttm3"),
];
#[derive(Clone, Copy)]
pub struct PersonalAccount {
    pub address: Pubkey,
    pub discriminator: [u8; 8],
}
pub const PERSONAL: [&[PersonalAccount]; 5] = [
    &[
        PersonalAccount {
            address: pubkey!("5GFk5KrtaEt3gZC8g3uLp4fJm566dy529JCWzqaFMsnZ"),
            discriminator: [88, 78, 124, 68, 228, 129, 34, 251],
        }, // LeveragePosition
    ],
    &[
        PersonalAccount {
            address: pubkey!("9uZYaDZrd8GFyTLTqetqXB5sesp7sgmJxhUtm8se6EwK"),
            discriminator: [233, 241, 119, 6, 2, 14, 106, 156],
        }, // YieldAccount
        PersonalAccount {
            address: pubkey!("AFGyBHZhJ7Kof8a1zoK7xnETn8FTPHZkdwTAPW5W5zKm"),
            discriminator: [233, 241, 119, 6, 2, 14, 106, 156],
        }, // YieldAccount
    ],
    &[
        PersonalAccount {
            address: pubkey!("6YoM5rhr1b44KF9ABZq1gmFJRRtcAM4RA1Ry217EBX1X"),
            discriminator: [233, 241, 119, 6, 2, 14, 106, 156],
        }, // YieldAccount
        PersonalAccount {
            address: pubkey!("6xBRotmJ9bJdURGqxiGg6Rt73MnyxwGP6JLemtr8LNjb"),
            discriminator: [233, 241, 119, 6, 2, 14, 106, 156],
        }, // YieldAccount
        PersonalAccount {
            address: pubkey!("8GxXF5BuejaKJJ7eFoQbPJN1UVXtHVtFpEnKMibxtiex"),
            discriminator: [233, 241, 119, 6, 2, 14, 106, 156],
        }, // YieldAccount
        PersonalAccount {
            address: pubkey!("8rzUxjrSfgfCNTbjJrSCKqUuxovZchZgszRUstH79vXo"),
            discriminator: [233, 241, 119, 6, 2, 14, 106, 156],
        }, // YieldAccount
        PersonalAccount {
            address: pubkey!("9GooFiPBB4BRByGLRJURhKs835NmkMyu7UQ4gUMowmRW"),
            discriminator: [186, 158, 208, 250, 7, 219, 13, 26],
        }, // JuniorExitAccount
        PersonalAccount {
            address: pubkey!("BEdrurdPxgigkAHB7dcbk3zpTEiK9ZfrndNTXhfrMCxU"),
            discriminator: [233, 241, 119, 6, 2, 14, 106, 156],
        }, // YieldAccount
        PersonalAccount {
            address: pubkey!("Bb6DpGAZow1thn3PkAEFoUd1tFHupLYexcahfeTGi5pA"),
            discriminator: [233, 241, 119, 6, 2, 14, 106, 156],
        }, // YieldAccount
        PersonalAccount {
            address: pubkey!("Dz4LK6u3W5JQfCjqd87tuYp66r4dQMRcEN4fAL9XS8Em"),
            discriminator: [243, 140, 20, 139, 32, 243, 114, 55],
        }, // BorrowPosition
        PersonalAccount {
            address: pubkey!("FgC339W8s8edjvQQS5yYSk5mvAqSsGidHzTgMBxRKcze"),
            discriminator: [88, 78, 124, 68, 228, 129, 34, 251],
        }, // LeveragePosition
    ],
    &[
        PersonalAccount {
            address: pubkey!("BGvsaog6aT9HPYhzijn5bgarUVqbWPHwa59mvxcAt2kc"),
            discriminator: [233, 241, 119, 6, 2, 14, 106, 156],
        }, // YieldAccount
        PersonalAccount {
            address: pubkey!("Ds55QyoE6nLPac5RkBf1zjChhmhGBLnKPqSXGXAovDog"),
            discriminator: [233, 241, 119, 6, 2, 14, 106, 156],
        }, // YieldAccount
        PersonalAccount {
            address: pubkey!("FPDNk2gffXtpSjoULhvPG4PpcKwyg1NZotLHnH7ww8FL"),
            discriminator: [233, 241, 119, 6, 2, 14, 106, 156],
        }, // YieldAccount
        PersonalAccount {
            address: pubkey!("H5d1hySUBKe8avcWkbk9z2kbbggU5otayixB13jhhh9y"),
            discriminator: [186, 158, 208, 250, 7, 219, 13, 26],
        }, // JuniorExitAccount
        PersonalAccount {
            address: pubkey!("YqV18MNFWRAHGEDuC4BMNufNE3zEHWgQcmHc5zJJWrq"),
            discriminator: [233, 241, 119, 6, 2, 14, 106, 156],
        }, // YieldAccount
    ],
    &[
        PersonalAccount {
            address: pubkey!("5X287p3iS4XDx659zGv6DPrw7fFbnrnpCF6nKmVKqUR4"),
            discriminator: [49, 60, 29, 23, 243, 219, 16, 214],
        }, // LeverageDelegation
        PersonalAccount {
            address: pubkey!("6AT92dBHWWnSyqkBxUvq3s3Bj8XRikvCMFBdca8yieVR"),
            discriminator: [233, 241, 119, 6, 2, 14, 106, 156],
        }, // YieldAccount
        PersonalAccount {
            address: pubkey!("8Au4wUtdSnrfyjZsPzrdbPjKvapMEwdiJ6GdZDGmder6"),
            discriminator: [233, 241, 119, 6, 2, 14, 106, 156],
        }, // YieldAccount
        PersonalAccount {
            address: pubkey!("9mSdg17r3GTfpwGRLaUzmmhE63g5AMRm5Sdo46yA7YwP"),
            discriminator: [88, 78, 124, 68, 228, 129, 34, 251],
        }, // LeveragePosition
        PersonalAccount {
            address: pubkey!("CwyUNfpYxEeKSqDFd2oRni9KE8poCQsBB8mpjFKVUr3N"),
            discriminator: [233, 241, 119, 6, 2, 14, 106, 156],
        }, // YieldAccount
        PersonalAccount {
            address: pubkey!("EfxEk52ZBdT6hFgZA77jCcppVWAcXvJpbT3L1FrizfRL"),
            discriminator: [243, 140, 20, 139, 32, 243, 114, 55],
        }, // BorrowPosition
        PersonalAccount {
            address: pubkey!("FqyeRBfcxywzDA6LsvM1s11KEQ2szAZPuwmG78X98xVh"),
            discriminator: [88, 78, 124, 68, 228, 129, 34, 251],
        }, // LeveragePosition
        PersonalAccount {
            address: pubkey!("G2WsgUNHkcchyzL8vVZx2zTtDsgz7feSoDA1Kf7pmgAM"),
            discriminator: [88, 78, 124, 68, 228, 129, 34, 251],
        }, // LeveragePosition
        PersonalAccount {
            address: pubkey!("Gr2pmCUtWqHFkcd4qaf1cxxcQLKRKsHTRPFAx9rSSVUr"),
            discriminator: [233, 241, 119, 6, 2, 14, 106, 156],
        }, // YieldAccount
    ],
];

pub const RECEIPT_MINTS: [Pubkey; 4] = [
    pubkey!("2xEJcAJSfJecvJtiB3sW4cXiQaUthU5BKkA9hJHzsrLP"),
    pubkey!("5hREvMCJ1Wa7ZHnnQhPoSQmqW3WFBFHrfF8MiFNBEjAa"),
    pubkey!("GhBwWP4FdyZ5Zor7sWMfzpuZZkeM9xvCCpmPpQ2eJhLP"),
    pubkey!("GMC7NrW6HsJpo7R8MMp3oVjRb8pP1ZJST2yoXr6nBhLP"),
];
pub const RECEIPT_ACCOUNTS: [Pubkey; 9] = [
    pubkey!("23q5BArzv2wdVowrB7TR4izcKD3Jp6bDbeymass4B5Gf"),
    pubkey!("3XJuGxkf23VEtyRfC5H1hf7WRDGnUjhHLuLuoEdVt5oJ"),
    pubkey!("3ySUgNrJ3jkbcEXKr1n1ALx37iLfUHq8zxPSstmH4yEp"),
    pubkey!("7FUYXTRXjhn4im6njqRDcgiwDmppGcg1HFdTuf3sXynJ"),
    pubkey!("7abpiNRH3B3immPFP1wHhUcZs3TNU1p2rPdVPoBDedDJ"),
    pubkey!("8GGeRnjNu83GNAwe7Tpe4UEjZTmyefh8CYDJQkj4qmjq"),
    pubkey!("BfHC37ZDHjp1kZjpYnh2FN68FbtdmHyJ9hR4bz8HDfR"),
    pubkey!("C2fKfcmxWBHGL2T4X7dt2BtYGAhZRpxDGTiRwmw62quz"),
    pubkey!("CpqPnWc3g8j93WFhQHoa5Bhv1TEMKzAcExSvAGdxnHT7"),
];
pub const RECEIPT_CUSTODY: [Pubkey; 1] = [pubkey!("7xf68VEvMtLJ2Up5zVZdA161uH1yNkTFYgDFhNUSwkM7")];
