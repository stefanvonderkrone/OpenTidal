//! Placeholder catalog data for the shell. These are hand-written stand-ins
//! until the app reads real Tidal data; the screen builders depend only on
//! these shapes, so swapping the source later is a one-file change.

#[derive(Clone, Copy)]
pub struct Album {
    pub title: &'static str,
    pub artist: &'static str,
}

#[derive(Clone, Copy)]
pub struct Artist {
    pub name: &'static str,
}

#[derive(Clone, Copy)]
pub struct Playlist {
    pub name: &'static str,
    pub description: &'static str,
}

#[derive(Clone, Copy)]
pub struct Track {
    pub title: &'static str,
    pub artist: &'static str,
    pub duration: &'static str,
}

pub const ALBUMS: [Album; 10] = [
    Album { title: "Currents", artist: "Tame Impala" },
    Album { title: "Blonde", artist: "Frank Ocean" },
    Album { title: "AM", artist: "Arctic Monkeys" },
    Album { title: "Random Access Memories", artist: "Daft Punk" },
    Album { title: "After Hours", artist: "The Weeknd" },
    Album { title: "DAMN.", artist: "Kendrick Lamar" },
    Album { title: "Vespertine", artist: "Björk" },
    Album { title: "In Rainbows", artist: "Radiohead" },
    Album { title: "Golden Hour", artist: "Kacey Musgraves" },
    Album { title: "A Seat at the Table", artist: "Solange" },
];

pub const ARTISTS: [Artist; 8] = [
    Artist { name: "Tame Impala" },
    Artist { name: "Frank Ocean" },
    Artist { name: "Arctic Monkeys" },
    Artist { name: "The Weeknd" },
    Artist { name: "Kendrick Lamar" },
    Artist { name: "Radiohead" },
    Artist { name: "Björk" },
    Artist { name: "Kacey Musgraves" },
];

pub const PLAYLISTS: [Playlist; 6] = [
    Playlist { name: "Chill", description: "Laid-back for slow evenings" },
    Playlist { name: "Workout", description: "Heavy and relentless" },
    Playlist { name: "Focus", description: "Instrumental and steady" },
    Playlist { name: "Road Trip", description: "Sing-along classics" },
    Playlist { name: "Late Night", description: "After-dark R&B" },
    Playlist { name: "Morning", description: "Bright starts" },
];

pub const TRACKS: [Track; 10] = [
    Track { title: "Blinding Lights", artist: "The Weeknd", duration: "3:20" },
    Track { title: "Let It Happen", artist: "Tame Impala", duration: "7:46" },
    Track { title: "Nikes", artist: "Frank Ocean", duration: "5:14" },
    Track { title: "Do I Wanna Know?", artist: "Arctic Monkeys", duration: "4:32" },
    Track { title: "Get Lucky", artist: "Daft Punk", duration: "6:09" },
    Track { title: "HUMBLE.", artist: "Kendrick Lamar", duration: "2:57" },
    Track { title: "Weird Fishes", artist: "Radiohead", duration: "5:18" },
    Track { title: "Hidden Place", artist: "Björk", duration: "5:28" },
    Track { title: "Slow Burn", artist: "Kacey Musgraves", duration: "4:06" },
    Track { title: "Cranes in the Sky", artist: "Solange", duration: "4:10" },
];
