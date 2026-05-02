// Binary protocol
//
// Request frame:
//   cmd 0x01=GET     [key_len:u32][key]
//   cmd 0x02=SET     [key_len:u32][key][val_len:u32][val]
//   cmd 0x03=DEL     [key_len:u32][key]
//   cmd 0x04=STATS
//   cmd 0x05=SET_VEC [key_len:u32][key][dim:u32][f32×dim]
//   cmd 0x06=SEARCH  [k:u32][dim:u32][f32×dim]
//   cmd 0x07=AUTH    [token_len:u32][token]
//
// Response frame:
//   status 0x00=OK  [payload_len:u32][payload]
//   status 0x01=NOT_FOUND [0:u32]
//   status 0x02=ERROR [msg_len:u32][msg]

use bytes::{Buf, BufMut, Bytes, BytesMut};

#[derive(Debug)]
pub enum Request {
    Get { key: Bytes },
    Set { key: Bytes, value: Bytes },
    Delete { key: Bytes },
    Stats,
    SetVec { key: Bytes, vector: Vec<f32> },
    Search { k: u32, query: Vec<f32> },
    Auth { token: Bytes },
}

#[derive(Debug)]
pub enum Response {
    Ok(Bytes),
    NotFound,
    Error(String),
}

pub fn encode_request(req: &Request) -> Bytes {
    let mut buf = BytesMut::new();
    match req {
        Request::Get { key } => {
            buf.put_u8(0x01);
            buf.put_u32(key.len() as u32);
            buf.put(key.as_ref());
        }
        Request::Set { key, value } => {
            buf.put_u8(0x02);
            buf.put_u32(key.len() as u32);
            buf.put(key.as_ref());
            buf.put_u32(value.len() as u32);
            buf.put(value.as_ref());
        }
        Request::Delete { key } => {
            buf.put_u8(0x03);
            buf.put_u32(key.len() as u32);
            buf.put(key.as_ref());
        }
        Request::Stats => {
            buf.put_u8(0x04);
        }
        Request::SetVec { key, vector } => {
            buf.put_u8(0x05);
            buf.put_u32(key.len() as u32);
            buf.put(key.as_ref());
            buf.put_u32(vector.len() as u32);
            for &f in vector {
                buf.put_f32(f);
            }
        }
        Request::Search { k, query } => {
            buf.put_u8(0x06);
            buf.put_u32(*k);
            buf.put_u32(query.len() as u32);
            for &f in query {
                buf.put_f32(f);
            }
        }
        Request::Auth { token } => {
            buf.put_u8(0x07);
            buf.put_u32(token.len() as u32);
            buf.put(token.as_ref());
        }
    }
    buf.freeze()
}

pub fn decode_request(buf: &mut BytesMut) -> Option<Request> {
    if buf.is_empty() {
        return None;
    }
    let cmd = buf[0];
    match cmd {
        0x01 => {
            if buf.len() < 5 {
                return None;
            }
            let key_len = u32::from_be_bytes(buf[1..5].try_into().unwrap()) as usize;
            if buf.len() < 5 + key_len {
                return None;
            }
            buf.advance(5);
            let key = buf.split_to(key_len).freeze();
            Some(Request::Get { key })
        }
        0x02 => {
            if buf.len() < 5 {
                return None;
            }
            let key_len = u32::from_be_bytes(buf[1..5].try_into().unwrap()) as usize;
            if buf.len() < 5 + key_len + 4 {
                return None;
            }
            let val_len =
                u32::from_be_bytes(buf[5 + key_len..9 + key_len].try_into().unwrap()) as usize;
            if buf.len() < 9 + key_len + val_len {
                return None;
            }
            buf.advance(5);
            let key = buf.split_to(key_len).freeze();
            buf.advance(4);
            let value = buf.split_to(val_len).freeze();
            Some(Request::Set { key, value })
        }
        0x03 => {
            if buf.len() < 5 {
                return None;
            }
            let key_len = u32::from_be_bytes(buf[1..5].try_into().unwrap()) as usize;
            if buf.len() < 5 + key_len {
                return None;
            }
            buf.advance(5);
            let key = buf.split_to(key_len).freeze();
            Some(Request::Delete { key })
        }
        0x04 => {
            buf.advance(1);
            Some(Request::Stats)
        }
        0x07 => {
            if buf.len() < 5 {
                return None;
            }
            let token_len = u32::from_be_bytes(buf[1..5].try_into().unwrap()) as usize;
            if buf.len() < 5 + token_len {
                return None;
            }
            buf.advance(5);
            let token = buf.split_to(token_len).freeze();
            Some(Request::Auth { token })
        }
        0x05 => {
            // SET_VEC: [key_len:u32][key][dim:u32][f32×dim]
            if buf.len() < 5 {
                return None;
            }
            let key_len = u32::from_be_bytes(buf[1..5].try_into().unwrap()) as usize;
            if buf.len() < 9 + key_len {
                return None;
            }
            let dim = u32::from_be_bytes(buf[5 + key_len..9 + key_len].try_into().unwrap()) as usize;
            if buf.len() < 9 + key_len + dim * 4 {
                return None;
            }
            buf.advance(5);
            let key = buf.split_to(key_len).freeze();
            buf.advance(4);
            let mut vector = Vec::with_capacity(dim);
            for _ in 0..dim {
                vector.push(buf.get_f32());
            }
            Some(Request::SetVec { key, vector })
        }
        0x06 => {
            // SEARCH: [k:u32][dim:u32][f32×dim]
            if buf.len() < 9 {
                return None;
            }
            let k = u32::from_be_bytes(buf[1..5].try_into().unwrap());
            let dim = u32::from_be_bytes(buf[5..9].try_into().unwrap()) as usize;
            if buf.len() < 9 + dim * 4 {
                return None;
            }
            buf.advance(9);
            let mut query = Vec::with_capacity(dim);
            for _ in 0..dim {
                query.push(buf.get_f32());
            }
            Some(Request::Search { k, query })
        }
        _ => {
            buf.advance(1);
            None
        }
    }
}

pub fn encode_response(resp: &Response) -> Bytes {
    let mut buf = BytesMut::new();
    match resp {
        Response::Ok(payload) => {
            buf.put_u8(0x00);
            buf.put_u32(payload.len() as u32);
            buf.put(payload.as_ref());
        }
        Response::NotFound => {
            buf.put_u8(0x01);
            buf.put_u32(0);
        }
        Response::Error(msg) => {
            buf.put_u8(0x02);
            buf.put_u32(msg.len() as u32);
            buf.put(msg.as_bytes());
        }
    }
    buf.freeze()
}

pub fn decode_response(buf: &mut BytesMut) -> Option<Response> {
    if buf.len() < 5 {
        return None;
    }
    let status = buf[0];
    let payload_len = u32::from_be_bytes(buf[1..5].try_into().unwrap()) as usize;
    if buf.len() < 5 + payload_len {
        return None;
    }
    buf.advance(5);
    let payload = buf.split_to(payload_len).freeze();

    match status {
        0x00 => Some(Response::Ok(payload)),
        0x01 => Some(Response::NotFound),
        0x02 => Some(Response::Error(
            String::from_utf8_lossy(&payload).into_owned(),
        )),
        _ => None,
    }
}

/// Encode search results into the Ok payload.
/// Format: [count:u32] + repeated [key_len:u32][key][score:f32]
pub fn encode_search_results(results: &[(String, f32)]) -> Bytes {
    let mut buf = BytesMut::new();
    buf.put_u32(results.len() as u32);
    for (key, score) in results {
        buf.put_u32(key.len() as u32);
        buf.put(key.as_bytes());
        buf.put_f32(*score);
    }
    buf.freeze()
}

/// Decode search results from an Ok payload.
pub fn decode_search_results(payload: &[u8]) -> Vec<(String, f32)> {
    if payload.len() < 4 {
        return vec![];
    }
    let count = u32::from_be_bytes(payload[0..4].try_into().unwrap()) as usize;
    let mut results = Vec::with_capacity(count);
    let mut cursor = 4;
    for _ in 0..count {
        if cursor + 4 > payload.len() {
            break;
        }
        let key_len = u32::from_be_bytes(payload[cursor..cursor + 4].try_into().unwrap()) as usize;
        cursor += 4;
        if cursor + key_len + 4 > payload.len() {
            break;
        }
        let key = String::from_utf8_lossy(&payload[cursor..cursor + key_len]).into_owned();
        cursor += key_len;
        let score = f32::from_be_bytes(payload[cursor..cursor + 4].try_into().unwrap());
        cursor += 4;
        results.push((key, score));
    }
    results
}
