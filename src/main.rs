use byteorder::{BigEndian, ReadBytesExt};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use thiserror::Error;

/// Represents a complete MIDI file
#[derive(Debug, Clone)]
pub struct MidiFile {
    pub header: MidiHeader,
    pub tracks: Vec<MidiTrack>,
}

/// MIDI file header information
#[derive(Debug, Clone)]
pub struct MidiHeader {
    pub format: u16,        // 0: single track, 1: multiple tracks, 2: multiple songs
    pub num_tracks: u16,    // Number of track chunks
    pub time_division: u16, // Timing information (ticks per quarter note or SMPTE format)
}

/// A single MIDI track containing events
#[derive(Debug, Clone)]
pub struct MidiTrack {
    pub events: Vec<MidiEvent>,
}

/// A MIDI event with timing information
#[derive(Debug, Clone)]
pub struct MidiEvent {
    pub delta_time: u32, // Time in ticks since previous event
    pub message: MidiMessage,
}

/// Different types of MIDI messages
#[derive(Debug, Clone)]
pub enum MidiMessage {
    NoteOn {
        channel: u8,
        note: u8,
        velocity: u8,
    },
    NoteOff {
        channel: u8,
        note: u8,
        velocity: u8,
    },
    PolyphonicKeyPressure {
        channel: u8,
        note: u8,
        pressure: u8,
    },
    ControlChange {
        channel: u8,
        controller: u8,
        value: u8,
    },
    ProgramChange {
        channel: u8,
        program: u8,
    },
    ChannelPressure {
        channel: u8,
        pressure: u8,
    },
    PitchBendChange {
        channel: u8,
        value: i16,
    },
    Meta(MetaEvent),
    SysEx(Vec<u8>),
}

/// MIDI meta events
#[derive(Debug, Clone)]
pub enum MetaEvent {
    SequenceNumber(u16),
    Text(String),
    CopyrightNotice(String),
    TrackName(String),
    InstrumentName(String),
    Lyrics(String),
    Marker(String),
    CuePoint(String),
    EndOfTrack,
    SetTempo(u32), // Microseconds per quarter note
    TimeSignature {
        numerator: u8,
        denominator: u8,
        clocks_per_metronome: u8,
        thirty_seconds_per_quarter: u8,
    },
    KeySignature {
        key: i8,   // -7 to 7 (negative = flats, positive = sharps)
        scale: u8, // 0 = major, 1 = minor
    },
    SequencerSpecific(Vec<u8>),
}

#[derive(Error, Debug)]
pub enum MidiError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    #[error("Invalid MIDI file: {0}")]
    Format(String),

    #[error("Unsupported MIDI feature: {0}")]
    Unsupported(String),
}

impl MidiFile {
    /// Open and parse a MIDI file from the given path
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, MidiError> {
        let mut file = File::open(path)?;

        // Parse header chunk
        Self::validate_chunk_header(&mut file, b"MThd")?;

        // Read header length (should be 6)
        let header_length = file.read_u32::<BigEndian>()?;
        if header_length != 6 {
            return Err(MidiError::Format(format!(
                "Invalid header length: {}",
                header_length
            )));
        }

        // Read header data
        let format = file.read_u16::<BigEndian>()?;
        let num_tracks = file.read_u16::<BigEndian>()?;
        let time_division = file.read_u16::<BigEndian>()?;

        // Check format is supported
        if format > 2 {
            return Err(MidiError::Format(format!(
                "Unsupported MIDI format: {}",
                format
            )));
        }

        let header = MidiHeader {
            format,
            num_tracks,
            time_division,
        };

        // Parse tracks
        let mut tracks = Vec::with_capacity(num_tracks as usize);
        for _ in 0..num_tracks {
            tracks.push(Self::parse_track(&mut file)?);
        }

        Ok(MidiFile { header, tracks })
    }

    /// Validate a chunk header matches the expected type
    fn validate_chunk_header(file: &mut File, expected: &[u8; 4]) -> Result<(), MidiError> {
        let mut chunk_type = [0u8; 4];
        file.read_exact(&mut chunk_type)?;

        if chunk_type != *expected {
            return Err(MidiError::Format(format!(
                "Expected chunk type {:?}, found {:?}",
                std::str::from_utf8(expected).unwrap_or("????"),
                std::str::from_utf8(&chunk_type).unwrap_or("????")
            )));
        }

        Ok(())
    }

    /// Parse a single MIDI track
    fn parse_track(file: &mut File) -> Result<MidiTrack, MidiError> {
        // Validate track header
        Self::validate_chunk_header(file, b"MTrk")?;

        // Read track length
        let track_length = file.read_u32::<BigEndian>()? as u64;
        let track_start_pos = file.stream_position()?;

        // Read all events in the track
        let mut events = Vec::new();
        let mut running_status = None;

        while file.stream_position()? < track_start_pos + track_length {
            let event = Self::parse_event(file, &mut running_status)?;
            events.push(event);

            // Check if we've reached an end of track event
            if let MidiMessage::Meta(MetaEvent::EndOfTrack) = events.last().unwrap().message {
                break;
            }
        }

        // Make sure we're at the correct position after track
        let current_pos = file.stream_position()?;
        let expected_pos = track_start_pos + track_length;
        if current_pos != expected_pos {
            file.seek(SeekFrom::Start(expected_pos))?;
        }

        Ok(MidiTrack { events })
    }

    /// Parse a single MIDI event
    fn parse_event(
        file: &mut File,
        running_status: &mut Option<u8>,
    ) -> Result<MidiEvent, MidiError> {
        // Read variable-length delta time
        let delta_time = Self::read_variable_length(file)?;

        // Read status byte or use running status
        let mut status = file.read_u8()?;

        // If the high bit is not set, this is data and we should use running status
        if status < 0x80 {
            if let Some(rs) = running_status {
                // Put back the byte we just read (it's actually data)
                file.seek(SeekFrom::Current(-1))?;
                status = *rs;
            } else {
                return Err(MidiError::Format(
                    "Unexpected data byte without running status".to_string(),
                ));
            }
        } else {
            // Update running status (except for System messages)
            if status < 0xF0 {
                *running_status = Some(status);
            }
        }

        // Parse message based on status byte
        let message = Self::parse_message(file, status)?;

        Ok(MidiEvent {
            delta_time,
            message,
        })
    }

    /// Parse a MIDI message based on its status byte
    fn parse_message(file: &mut File, status: u8) -> Result<MidiMessage, MidiError> {
        match status {
            // Note Off: 0x80-0x8F
            0x80..=0x8F => {
                let channel = status & 0x0F;
                let note = file.read_u8()?;
                let velocity = file.read_u8()?;
                Ok(MidiMessage::NoteOff {
                    channel,
                    note,
                    velocity,
                })
            }

            // Note On: 0x90-0x9F
            0x90..=0x9F => {
                let channel = status & 0x0F;
                let note = file.read_u8()?;
                let velocity = file.read_u8()?;
                // Note-on with velocity 0 is equivalent to note-off
                if velocity == 0 {
                    Ok(MidiMessage::NoteOff {
                        channel,
                        note,
                        velocity,
                    })
                } else {
                    Ok(MidiMessage::NoteOn {
                        channel,
                        note,
                        velocity,
                    })
                }
            }

            // Polyphonic Key Pressure: 0xA0-0xAF
            0xA0..=0xAF => {
                let channel = status & 0x0F;
                let note = file.read_u8()?;
                let pressure = file.read_u8()?;
                Ok(MidiMessage::PolyphonicKeyPressure {
                    channel,
                    note,
                    pressure,
                })
            }

            // Control Change: 0xB0-0xBF
            0xB0..=0xBF => {
                let channel = status & 0x0F;
                let controller = file.read_u8()?;
                let value = file.read_u8()?;
                Ok(MidiMessage::ControlChange {
                    channel,
                    controller,
                    value,
                })
            }

            // Program Change: 0xC0-0xCF
            0xC0..=0xCF => {
                let channel = status & 0x0F;
                let program = file.read_u8()?;
                Ok(MidiMessage::ProgramChange { channel, program })
            }

            // Channel Pressure: 0xD0-0xDF
            0xD0..=0xDF => {
                let channel = status & 0x0F;
                let pressure = file.read_u8()?;
                Ok(MidiMessage::ChannelPressure { channel, pressure })
            }

            // Pitch Bend: 0xE0-0xEF
            0xE0..=0xEF => {
                let channel = status & 0x0F;
                let lsb = file.read_u8()? as u16;
                let msb = file.read_u8()? as u16;
                let value = ((msb << 7) | lsb) as i16 - 8192; // Center value at 0
                Ok(MidiMessage::PitchBendChange { channel, value })
            }

            // System Exclusive: 0xF0
            0xF0 => {
                let mut data = Vec::new();
                loop {
                    let byte = file.read_u8()?;
                    if byte == 0xF7 {
                        break;
                    } // End of SysEx
                    data.push(byte);
                }
                Ok(MidiMessage::SysEx(data))
            }

            // Meta Event: 0xFF
            0xFF => {
                let meta_type = file.read_u8()?;
                let length = Self::read_variable_length(file)?;
                let mut data = vec![0; length as usize];
                file.read_exact(&mut data)?;

                match meta_type {
                    0x00 => {
                        if length != 2 {
                            return Err(MidiError::Format(
                                "Invalid sequence number length".to_string(),
                            ));
                        }
                        let value = ((data[0] as u16) << 8) | (data[1] as u16);
                        Ok(MidiMessage::Meta(MetaEvent::SequenceNumber(value)))
                    }
                    0x01 => Ok(MidiMessage::Meta(MetaEvent::Text(
                        String::from_utf8_lossy(&data).into_owned(),
                    ))),
                    0x02 => Ok(MidiMessage::Meta(MetaEvent::CopyrightNotice(
                        String::from_utf8_lossy(&data).into_owned(),
                    ))),
                    0x03 => Ok(MidiMessage::Meta(MetaEvent::TrackName(
                        String::from_utf8_lossy(&data).into_owned(),
                    ))),
                    0x04 => Ok(MidiMessage::Meta(MetaEvent::InstrumentName(
                        String::from_utf8_lossy(&data).into_owned(),
                    ))),
                    0x05 => Ok(MidiMessage::Meta(MetaEvent::Lyrics(
                        String::from_utf8_lossy(&data).into_owned(),
                    ))),
                    0x06 => Ok(MidiMessage::Meta(MetaEvent::Marker(
                        String::from_utf8_lossy(&data).into_owned(),
                    ))),
                    0x07 => Ok(MidiMessage::Meta(MetaEvent::CuePoint(
                        String::from_utf8_lossy(&data).into_owned(),
                    ))),
                    0x2F => {
                        if length != 0 {
                            return Err(MidiError::Format(
                                "End of track event with non-zero length".to_string(),
                            ));
                        }
                        Ok(MidiMessage::Meta(MetaEvent::EndOfTrack))
                    }
                    0x51 => {
                        if length != 3 {
                            return Err(MidiError::Format(
                                "Invalid tempo event length".to_string(),
                            ));
                        }
                        let tempo =
                            ((data[0] as u32) << 16) | ((data[1] as u32) << 8) | (data[2] as u32);
                        Ok(MidiMessage::Meta(MetaEvent::SetTempo(tempo)))
                    }
                    0x58 => {
                        if length != 4 {
                            return Err(MidiError::Format(
                                "Invalid time signature length".to_string(),
                            ));
                        }
                        Ok(MidiMessage::Meta(MetaEvent::TimeSignature {
                            numerator: data[0],
                            denominator: 1 << data[1], // 2^n
                            clocks_per_metronome: data[2],
                            thirty_seconds_per_quarter: data[3],
                        }))
                    }
                    0x59 => {
                        if length != 2 {
                            return Err(MidiError::Format(
                                "Invalid key signature length".to_string(),
                            ));
                        }
                        Ok(MidiMessage::Meta(MetaEvent::KeySignature {
                            key: data[0] as i8,
                            scale: data[1],
                        }))
                    }
                    0x7F => Ok(MidiMessage::Meta(MetaEvent::SequencerSpecific(data))),
                    _ => Err(MidiError::Unsupported(format!(
                        "Unsupported meta event type: {}",
                        meta_type
                    ))),
                }
            }

            // Unsupported message type
            _ => Err(MidiError::Unsupported(format!(
                "Unsupported MIDI message type: 0x{:02X}",
                status
            ))),
        }
    }

    /// Read a variable-length quantity
    fn read_variable_length(file: &mut File) -> Result<u32, MidiError> {
        let mut value: u32 = 0;
        loop {
            let byte = file.read_u8()?;
            value = (value << 7) | (byte & 0x7F) as u32;
            if byte & 0x80 == 0 {
                break;
            }
        }
        Ok(value)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();

    // Check if a path was provided as an argument
    let path = if args.len() > 1 {
        &args[1]
    } else {
        // Default to a sample path if none provided
        "example.mid"
    };

    println!("Opening MIDI file: {}", path);

    // Open and parse the MIDI file
    let midi_file = MidiFile::open(path)?;

    // Display MIDI file information
    println!("MIDI file format: {}", midi_file.header.format);
    println!("Number of tracks: {}", midi_file.header.num_tracks);
    println!("Time division: {}", midi_file.header.time_division);

    // Display information about each track
    for (i, track) in midi_file.tracks.iter().enumerate() {
        println!("Track {}: {} events", i, track.events.len());

        // Count note events (optional)
        let note_on_count = track
            .events
            .iter()
            .filter(|e| matches!(e.message, MidiMessage::NoteOn { .. }))
            .count();

        println!("  Contains {} note-on events", note_on_count);
    }

    Ok(())
}
