use std::io;

pub fn encode_pkt_line(data: &[u8]) -> Vec<u8> {
    let length = data.len() + 4;

    let mut packet = format!("{length:04x}").into_bytes();
    packet.extend_from_slice(data);

    packet
}

pub fn parse_pkt_lines(data: &[u8]) -> io::Result<Vec<Vec<u8>>> {
    let mut packets = Vec::new();
    let mut cursor = 0;

    while cursor < data.len() {
        if cursor + 4 > data.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Incomplete packet header",
            ));
        }

        let length_str = std::str::from_utf8(&data[cursor..cursor + 4])
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid packet length"))?;

        let length = usize::from_str_radix(length_str, 16)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid packet length"))?;

        cursor += 4;

        match length {
            0 | 1 | 2 => {
                // Flush, delimiter, response-end.
                continue;
            }
            _ => {}
        }

        if length < 4 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid packet length",
            ));
        }

        let payload_length = length - 4;
        let end = cursor
            .checked_add(payload_length)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Packet length overflow"))?;

        let payload = data.get(cursor..end).ok_or_else(|| {
            io::Error::new(io::ErrorKind::UnexpectedEof, "Incomplete packet payload")
        })?;

        packets.push(payload.to_vec());

        cursor = end;
    }

    Ok(packets)
}
