// Generated from PCSG00933 eboot.bin.elf static sector tables.
pub const SECTOR_SIZE: usize = 0x800;
pub const ADDPT_SECTORS: u16 = 501;

pub const PT_SECTORS: [u16; 23] = [
    1069, 1, 391, 518, 525, 2103, 488, 1388, 1581, 6, 7, 106, 57, 1, 8, 62,
    488, 231, 131, 2, 23, 13, 2,
];

pub const SC_SECTORS: [u16; 89] = [
    8, 10, 10, 12, 12, 14, 10, 10, 36, 12, 18, 14, 36, 14, 38, 20,
    22, 26, 24, 26, 10, 20, 34, 18, 18, 38, 20, 20, 20, 30, 40, 18,
    22, 10, 10, 12, 22, 16, 16, 14, 14, 12, 20, 16, 14, 30, 16, 22,
    12, 10, 18, 12, 20, 22, 24, 10, 10, 10, 8, 10, 10, 8, 10, 12,
    12, 10, 10, 8, 10, 10, 8, 8, 8, 24, 10, 8, 8, 10, 8, 8,
    10, 8, 8, 8, 10, 10, 30, 14, 6,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScMetadata {
    pub stream_base: u16,
    pub stream_count: u16,
    pub primary_base: u16,
    pub primary_count: u16,
    pub secondary_base: u16,
    pub secondary_count: u16,
}

// Executable-resident SC metadata at 0x810F9B1C. These records map each
// local compiled-script table into global runtime state; they do not contain
// the script payload itself.
pub const SC_METADATA: [ScMetadata; 89] = [
    ScMetadata { stream_base: 0, stream_count: 3, primary_base: 0, primary_count: 69, secondary_base: 0, secondary_count: 0 },
    ScMetadata { stream_base: 3, stream_count: 3, primary_base: 9, primary_count: 89, secondary_base: 0, secondary_count: 0 },
    ScMetadata { stream_base: 6, stream_count: 3, primary_base: 21, primary_count: 78, secondary_base: 0, secondary_count: 0 },
    ScMetadata { stream_base: 9, stream_count: 3, primary_base: 31, primary_count: 117, secondary_base: 0, secondary_count: 0 },
    ScMetadata { stream_base: 12, stream_count: 3, primary_base: 46, primary_count: 115, secondary_base: 0, secondary_count: 0 },
    ScMetadata { stream_base: 15, stream_count: 2, primary_base: 61, primary_count: 160, secondary_base: 0, secondary_count: 0 },
    ScMetadata { stream_base: 17, stream_count: 3, primary_base: 81, primary_count: 115, secondary_base: 0, secondary_count: 0 },
    ScMetadata { stream_base: 20, stream_count: 3, primary_base: 96, primary_count: 100, secondary_base: 0, secondary_count: 0 },
    ScMetadata { stream_base: 23, stream_count: 25, primary_base: 109, primary_count: 683, secondary_base: 0, secondary_count: 4 },
    ScMetadata { stream_base: 48, stream_count: 10, primary_base: 195, primary_count: 170, secondary_base: 4, secondary_count: 2 },
    ScMetadata { stream_base: 58, stream_count: 12, primary_base: 217, primary_count: 313, secondary_base: 6, secondary_count: 2 },
    ScMetadata { stream_base: 70, stream_count: 6, primary_base: 257, primary_count: 193, secondary_base: 8, secondary_count: 1 },
    ScMetadata { stream_base: 76, stream_count: 22, primary_base: 282, primary_count: 683, secondary_base: 9, secondary_count: 2 },
    ScMetadata { stream_base: 98, stream_count: 7, primary_base: 368, primary_count: 186, secondary_base: 11, secondary_count: 1 },
    ScMetadata { stream_base: 105, stream_count: 24, primary_base: 392, primary_count: 689, secondary_base: 12, secondary_count: 3 },
    ScMetadata { stream_base: 129, stream_count: 2, primary_base: 479, primary_count: 276, secondary_base: 15, secondary_count: 0 },
    ScMetadata { stream_base: 131, stream_count: 6, primary_base: 514, primary_count: 347, secondary_base: 15, secondary_count: 1 },
    ScMetadata { stream_base: 137, stream_count: 25, primary_base: 558, primary_count: 397, secondary_base: 16, secondary_count: 3 },
    ScMetadata { stream_base: 162, stream_count: 11, primary_base: 608, primary_count: 407, secondary_base: 19, secondary_count: 1 },
    ScMetadata { stream_base: 173, stream_count: 13, primary_base: 659, primary_count: 485, secondary_base: 20, secondary_count: 2 },
    ScMetadata { stream_base: 186, stream_count: 2, primary_base: 720, primary_count: 127, secondary_base: 22, secondary_count: 0 },
    ScMetadata { stream_base: 188, stream_count: 7, primary_base: 736, primary_count: 325, secondary_base: 22, secondary_count: 1 },
    ScMetadata { stream_base: 195, stream_count: 23, primary_base: 777, primary_count: 609, secondary_base: 23, secondary_count: 3 },
    ScMetadata { stream_base: 218, stream_count: 2, primary_base: 854, primary_count: 271, secondary_base: 26, secondary_count: 0 },
    ScMetadata { stream_base: 220, stream_count: 2, primary_base: 888, primary_count: 265, secondary_base: 26, secondary_count: 0 },
    ScMetadata { stream_base: 222, stream_count: 21, primary_base: 922, primary_count: 687, secondary_base: 26, secondary_count: 2 },
    ScMetadata { stream_base: 243, stream_count: 6, primary_base: 1008, primary_count: 320, secondary_base: 28, secondary_count: 1 },
    ScMetadata { stream_base: 249, stream_count: 5, primary_base: 1048, primary_count: 308, secondary_base: 29, secondary_count: 0 },
    ScMetadata { stream_base: 254, stream_count: 18, primary_base: 1087, primary_count: 337, secondary_base: 29, secondary_count: 2 },
    ScMetadata { stream_base: 272, stream_count: 13, primary_base: 1130, primary_count: 532, secondary_base: 31, secondary_count: 1 },
    ScMetadata { stream_base: 285, stream_count: 25, primary_base: 1197, primary_count: 721, secondary_base: 32, secondary_count: 3 },
    ScMetadata { stream_base: 310, stream_count: 2, primary_base: 1288, primary_count: 277, secondary_base: 35, secondary_count: 0 },
    ScMetadata { stream_base: 312, stream_count: 5, primary_base: 1323, primary_count: 377, secondary_base: 35, secondary_count: 0 },
    ScMetadata { stream_base: 317, stream_count: 6, primary_base: 1371, primary_count: 118, secondary_base: 35, secondary_count: 1 },
    ScMetadata { stream_base: 323, stream_count: 2, primary_base: 1386, primary_count: 122, secondary_base: 36, secondary_count: 0 },
    ScMetadata { stream_base: 325, stream_count: 2, primary_base: 1402, primary_count: 155, secondary_base: 36, secondary_count: 0 },
    ScMetadata { stream_base: 327, stream_count: 14, primary_base: 1422, primary_count: 388, secondary_base: 36, secondary_count: 0 },
    ScMetadata { stream_base: 341, stream_count: 10, primary_base: 1471, primary_count: 280, secondary_base: 36, secondary_count: 1 },
    ScMetadata { stream_base: 351, stream_count: 7, primary_base: 1506, primary_count: 285, secondary_base: 37, secondary_count: 1 },
    ScMetadata { stream_base: 358, stream_count: 2, primary_base: 1542, primary_count: 200, secondary_base: 38, secondary_count: 0 },
    ScMetadata { stream_base: 360, stream_count: 2, primary_base: 1567, primary_count: 194, secondary_base: 38, secondary_count: 0 },
    ScMetadata { stream_base: 362, stream_count: 2, primary_base: 1592, primary_count: 145, secondary_base: 38, secondary_count: 0 },
    ScMetadata { stream_base: 364, stream_count: 7, primary_base: 1611, primary_count: 343, secondary_base: 38, secondary_count: 1 },
    ScMetadata { stream_base: 371, stream_count: 5, primary_base: 1654, primary_count: 248, secondary_base: 39, secondary_count: 0 },
    ScMetadata { stream_base: 376, stream_count: 4, primary_base: 1685, primary_count: 209, secondary_base: 39, secondary_count: 0 },
    ScMetadata { stream_base: 380, stream_count: 12, primary_base: 1712, primary_count: 620, secondary_base: 39, secondary_count: 2 },
    ScMetadata { stream_base: 392, stream_count: 2, primary_base: 1790, primary_count: 283, secondary_base: 41, secondary_count: 0 },
    ScMetadata { stream_base: 394, stream_count: 2, primary_base: 1826, primary_count: 413, secondary_base: 41, secondary_count: 0 },
    ScMetadata { stream_base: 396, stream_count: 2, primary_base: 1878, primary_count: 140, secondary_base: 41, secondary_count: 0 },
    ScMetadata { stream_base: 398, stream_count: 2, primary_base: 1896, primary_count: 92, secondary_base: 41, secondary_count: 0 },
    ScMetadata { stream_base: 400, stream_count: 2, primary_base: 1908, primary_count: 306, secondary_base: 41, secondary_count: 0 },
    ScMetadata { stream_base: 402, stream_count: 2, primary_base: 1947, primary_count: 153, secondary_base: 41, secondary_count: 0 },
    ScMetadata { stream_base: 404, stream_count: 7, primary_base: 1967, primary_count: 370, secondary_base: 41, secondary_count: 1 },
    ScMetadata { stream_base: 411, stream_count: 2, primary_base: 2014, primary_count: 404, secondary_base: 42, secondary_count: 0 },
    ScMetadata { stream_base: 413, stream_count: 12, primary_base: 2065, primary_count: 429, secondary_base: 42, secondary_count: 0 },
    ScMetadata { stream_base: 425, stream_count: 2, primary_base: 2119, primary_count: 85, secondary_base: 42, secondary_count: 0 },
    ScMetadata { stream_base: 427, stream_count: 2, primary_base: 2130, primary_count: 104, secondary_base: 42, secondary_count: 0 },
    ScMetadata { stream_base: 429, stream_count: 2, primary_base: 2143, primary_count: 89, secondary_base: 42, secondary_count: 0 },
    ScMetadata { stream_base: 431, stream_count: 2, primary_base: 2155, primary_count: 69, secondary_base: 42, secondary_count: 0 },
    ScMetadata { stream_base: 433, stream_count: 2, primary_base: 2164, primary_count: 114, secondary_base: 42, secondary_count: 0 },
    ScMetadata { stream_base: 435, stream_count: 2, primary_base: 2179, primary_count: 107, secondary_base: 42, secondary_count: 0 },
    ScMetadata { stream_base: 437, stream_count: 2, primary_base: 2193, primary_count: 78, secondary_base: 42, secondary_count: 0 },
    ScMetadata { stream_base: 439, stream_count: 2, primary_base: 2203, primary_count: 119, secondary_base: 42, secondary_count: 0 },
    ScMetadata { stream_base: 441, stream_count: 2, primary_base: 2218, primary_count: 132, secondary_base: 42, secondary_count: 0 },
    ScMetadata { stream_base: 443, stream_count: 2, primary_base: 2235, primary_count: 140, secondary_base: 42, secondary_count: 0 },
    ScMetadata { stream_base: 445, stream_count: 2, primary_base: 2253, primary_count: 120, secondary_base: 42, secondary_count: 0 },
    ScMetadata { stream_base: 447, stream_count: 2, primary_base: 2268, primary_count: 114, secondary_base: 42, secondary_count: 0 },
    ScMetadata { stream_base: 449, stream_count: 2, primary_base: 2283, primary_count: 97, secondary_base: 42, secondary_count: 0 },
    ScMetadata { stream_base: 451, stream_count: 2, primary_base: 2296, primary_count: 100, secondary_base: 42, secondary_count: 0 },
    ScMetadata { stream_base: 453, stream_count: 3, primary_base: 2309, primary_count: 94, secondary_base: 42, secondary_count: 0 },
    ScMetadata { stream_base: 456, stream_count: 3, primary_base: 2321, primary_count: 73, secondary_base: 42, secondary_count: 0 },
    ScMetadata { stream_base: 459, stream_count: 3, primary_base: 2331, primary_count: 77, secondary_base: 42, secondary_count: 0 },
    ScMetadata { stream_base: 462, stream_count: 3, primary_base: 2341, primary_count: 60, secondary_base: 42, secondary_count: 0 },
    ScMetadata { stream_base: 465, stream_count: 26, primary_base: 2349, primary_count: 430, secondary_base: 42, secondary_count: 0 },
    ScMetadata { stream_base: 491, stream_count: 3, primary_base: 2403, primary_count: 89, secondary_base: 42, secondary_count: 0 },
    ScMetadata { stream_base: 494, stream_count: 3, primary_base: 2415, primary_count: 62, secondary_base: 42, secondary_count: 0 },
    ScMetadata { stream_base: 497, stream_count: 3, primary_base: 2423, primary_count: 81, secondary_base: 42, secondary_count: 0 },
    ScMetadata { stream_base: 500, stream_count: 3, primary_base: 2434, primary_count: 93, secondary_base: 42, secondary_count: 0 },
    ScMetadata { stream_base: 503, stream_count: 3, primary_base: 2446, primary_count: 81, secondary_base: 42, secondary_count: 0 },
    ScMetadata { stream_base: 506, stream_count: 3, primary_base: 2457, primary_count: 86, secondary_base: 42, secondary_count: 0 },
    ScMetadata { stream_base: 509, stream_count: 3, primary_base: 2468, primary_count: 81, secondary_base: 42, secondary_count: 0 },
    ScMetadata { stream_base: 512, stream_count: 3, primary_base: 2479, primary_count: 43, secondary_base: 42, secondary_count: 0 },
    ScMetadata { stream_base: 515, stream_count: 3, primary_base: 2485, primary_count: 82, secondary_base: 42, secondary_count: 0 },
    ScMetadata { stream_base: 518, stream_count: 3, primary_base: 2496, primary_count: 68, secondary_base: 42, secondary_count: 0 },
    ScMetadata { stream_base: 521, stream_count: 3, primary_base: 2505, primary_count: 102, secondary_base: 42, secondary_count: 0 },
    ScMetadata { stream_base: 524, stream_count: 3, primary_base: 2518, primary_count: 97, secondary_base: 42, secondary_count: 0 },
    ScMetadata { stream_base: 527, stream_count: 6, primary_base: 2531, primary_count: 492, secondary_base: 42, secondary_count: 1 },
    ScMetadata { stream_base: 533, stream_count: 184, primary_base: 2593, primary_count: 284, secondary_base: 43, secondary_count: 3 },
    ScMetadata { stream_base: 717, stream_count: 6, primary_base: 2629, primary_count: 18, secondary_base: 46, secondary_count: 1 },
];

pub fn sc_metadata(id: u32) -> Option<ScMetadata> {
    SC_METADATA.get(usize::try_from(id).ok()?).copied()
}

pub const BK_SECTORS: [u16; 326] = [
    427, 554, 448, 442, 409, 405, 418, 463, 511, 511, 510, 366, 365, 365, 516, 9,
    531, 530, 529, 448, 595, 324, 320, 565, 363, 409, 479, 554, 554, 554, 556, 378,
    458, 517, 535, 530, 537, 521, 601, 257, 263, 437, 436, 438, 437, 432, 433, 553,
    633, 633, 531, 531, 539, 543, 545, 521, 451, 398, 552, 410, 635, 515, 518, 513,
    518, 515, 494, 390, 389, 556, 568, 431, 583, 410, 377, 592, 469, 347, 535, 411,
    426, 402, 9, 422, 454, 639, 639, 510, 510, 510, 459, 512, 480, 541, 541, 529,
    423, 422, 423, 393, 626, 560, 526, 510, 365, 415, 415, 415, 563, 469, 495, 494,
    452, 435, 434, 496, 419, 305, 518, 527, 598, 618, 408, 483, 531, 486, 522, 348,
    366, 380, 346, 389, 400, 311, 331, 543, 99, 475, 543, 394, 430, 455, 430, 378,
    434, 415, 357, 264, 612, 475, 637, 319, 629, 544, 400, 615, 528, 627, 342, 522,
    424, 540, 559, 488, 508, 608, 555, 617, 534, 467, 537, 543, 532, 505, 542, 492,
    559, 603, 512, 606, 556, 502, 516, 637, 462, 558, 518, 367, 568, 528, 377, 426,
    448, 319, 467, 513, 320, 631, 617, 563, 719, 726, 509, 617, 623, 454, 484, 388,
    462, 516, 531, 416, 718, 671, 546, 594, 603, 518, 642, 576, 498, 412, 382, 338,
    452, 577, 614, 420, 636, 477, 618, 628, 602, 522, 515, 457, 313, 345, 347, 292,
    389, 388, 389, 389, 388, 273, 329, 374, 418, 428, 344, 406, 426, 399, 9, 9,
    258, 283, 310, 257, 303, 306, 263, 310, 657, 628, 426, 89, 89, 89, 89, 89,
    89, 397, 294, 375, 351, 277, 230, 269, 312, 459, 423, 277, 187, 325, 516, 518,
    558, 373, 541, 366, 229, 324, 323, 240, 433, 293, 430, 277, 220, 516, 388, 522,
    387, 566, 565, 566, 568, 566, 89, 568, 89, 566, 566, 566, 566, 566, 89, 564,
    565, 20, 13, 17, 549, 546,
];

pub const BSF_SECTORS: [u16; 294] = [
    88, 78, 78, 89, 78, 78, 85, 75, 80, 86, 75, 81, 58, 58, 62, 59,
    60, 63, 55, 56, 59, 56, 58, 60, 53, 52, 63, 54, 53, 63, 53, 53,
    52, 58, 54, 52, 59, 53, 84, 91, 81, 86, 93, 87, 79, 90, 82, 81,
    88, 84, 60, 66, 72, 61, 67, 74, 66, 74, 79, 67, 74, 81, 88, 85,
    83, 89, 85, 83, 70, 72, 71, 71, 72, 71, 73, 77, 72, 74, 78, 75,
    64, 65, 63, 66, 66, 65, 73, 67, 74, 74, 68, 76, 69, 66, 69, 70,
    68, 71, 62, 50, 62, 50, 24, 19, 86, 96, 89, 99, 80, 83, 84, 86,
    110, 110, 111, 114, 120, 118, 118, 121, 113, 113, 113, 116, 124, 121, 121, 124,
    74, 77, 74, 77, 87, 83, 88, 83, 88, 70, 91, 73, 81, 82, 84, 85,
    91, 91, 91, 91, 82, 82, 43, 43, 91, 93, 62, 64, 64, 62, 61, 64,
    56, 50, 58, 56, 50, 57, 46, 50, 50, 46, 50, 51, 42, 44, 44, 41,
    43, 43, 45, 44, 48, 45, 44, 48, 44, 42, 40, 44, 41, 40, 42, 40,
    55, 63, 55, 53, 60, 56, 51, 52, 52, 49, 53, 50, 50, 50, 51, 48,
    48, 50, 47, 48, 49, 45, 46, 47, 81, 70, 72, 77, 71, 70, 51, 48,
    46, 49, 47, 46, 65, 62, 59, 62, 62, 57, 50, 50, 46, 51, 50, 45,
    52, 50, 56, 52, 49, 57, 51, 48, 51, 51, 47, 52, 24, 19, 24, 19,
    11, 10, 74, 68, 71, 69, 62, 59, 63, 60, 115, 117, 117, 119, 66, 59,
    66, 59, 63, 69, 63, 65, 57, 51, 58, 50, 64, 66, 63, 64, 91, 91,
    72, 72, 43, 43, 60, 60,
];

pub fn allocation_size(archive_name: &str, id: u32) -> Option<usize> {
    let sectors = match archive_name {
        "addpt.cpk" if id == 0 => ADDPT_SECTORS,
        "pt.cpk" => *PT_SECTORS.get(usize::try_from(id).ok()?)?,
        "sc.cpk" => *SC_SECTORS.get(usize::try_from(id).ok()?)?,
        "bk.cpk" => *BK_SECTORS.get(usize::try_from(id).ok()?)?,
        "bsf.cpk" => *BSF_SECTORS.get(usize::try_from(id).ok()?)?,
        _ => return None,
    };
    usize::from(sectors).checked_mul(SECTOR_SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executable_table_lengths_match_registry_counts() {
        assert_eq!(SC_SECTORS.len(), 0x59);
        assert_eq!(BK_SECTORS.len(), 0x146);
        assert_eq!(BSF_SECTORS.len(), 0x126);
        assert_eq!(PT_SECTORS.len(), 0x17);
    }

    #[test]
    fn known_placeholder_allocations_are_one_sector() {
        assert_eq!(allocation_size("pt.cpk", 1), Some(0x800));
        assert_eq!(allocation_size("pt.cpk", 13), Some(0x800));
    }
}
