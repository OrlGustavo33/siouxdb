//! Data e hora: microssegundos desde 1970-01-01 00:00:00 UTC, num inteiro de 64 bits.
//! Texto no formato ISO: `2026-11-23`, `2026-11-23 14:30:00`, `2026-11-23T14:30:00.123456Z`,
//! `2026-11-23T11:30:00-03:00` (o deslocamento é aplicado e o valor guardado fica em UTC).

const MICROS_DIA: i64 = 86_400_000_000;

/// Dias desde 1970-01-01 para uma data do calendário gregoriano (algoritmo de H. Hinnant).
fn dias_de_data(ano: i64, mes: i64, dia: i64) -> i64 {
    let a = if mes <= 2 { ano - 1 } else { ano };
    let era = a.div_euclid(400);
    let ano_da_era = a.rem_euclid(400);
    let dia_do_ano = (153 * (if mes > 2 { mes - 3 } else { mes + 9 }) + 2) / 5 + dia - 1;
    let dia_da_era = ano_da_era * 365 + ano_da_era / 4 - ano_da_era / 100 + dia_do_ano;
    era * 146_097 + dia_da_era - 719_468
}

fn data_de_dias(dias: i64) -> (i64, i64, i64) {
    let z = dias + 719_468;
    let era = z.div_euclid(146_097);
    let dia_da_era = z.rem_euclid(146_097);
    let ano_da_era =
        (dia_da_era - dia_da_era / 1460 + dia_da_era / 36_524 - dia_da_era / 146_096) / 365;
    let dia_do_ano = dia_da_era - (365 * ano_da_era + ano_da_era / 4 - ano_da_era / 100);
    let mp = (5 * dia_do_ano + 2) / 153;
    let dia = dia_do_ano - (153 * mp + 2) / 5 + 1;
    let mes = if mp < 10 { mp + 3 } else { mp - 9 };
    (ano_da_era + era * 400 + i64::from(mes <= 2), mes, dia)
}

fn dias_no_mes(ano: i64, mes: i64) -> i64 {
    match mes {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (ano % 4 == 0 && ano % 100 != 0) || ano % 400 == 0 => 29,
        _ => 28,
    }
}

fn numero(s: &str, tamanho: usize) -> Option<i64> {
    (s.len() == tamanho && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

/// Lê uma data (ou data e hora) em ISO. `None` se o texto não for uma data válida.
pub fn de_texto(s: &str) -> Option<i64> {
    let s = s.trim();
    let (data, resto) = if s.len() > 10 {
        (s.get(..10)?, s.get(10..)?)
    } else {
        (s, "")
    };
    let mut partes = data.split('-');
    let (ano, mes, dia) = (
        numero(partes.next()?, 4)?,
        numero(partes.next()?, 2)?,
        numero(partes.next()?, 2)?,
    );
    if partes.next().is_some()
        || ano < 1
        || !(1..=12).contains(&mes)
        || dia < 1
        || dia > dias_no_mes(ano, mes)
    {
        return None;
    }
    let mut micros = dias_de_data(ano, mes, dia) * MICROS_DIA;
    if resto.is_empty() {
        return Some(micros);
    }
    let resto = resto.strip_prefix(['T', 't', ' '])?;
    // Fuso: Z, +HH:MM ou -HH:MM no fim.
    let (hora, deslocamento) = if let Some(h) = resto.strip_suffix(['Z', 'z']) {
        (h, 0)
    } else if let Some(pos) = resto.rfind(['+', '-']) {
        let (h, fuso) = resto.split_at(pos);
        let (fh, fm) = fuso[1..].split_once(':')?;
        let minutos = numero(fh, 2)? * 60 + numero(fm, 2)?;
        if minutos > 14 * 60 {
            return None;
        }
        (
            h,
            if fuso.starts_with('-') {
                -minutos
            } else {
                minutos
            },
        )
    } else {
        (resto, 0)
    };
    let (hms, fracao) = hora.split_once('.').unwrap_or((hora, ""));
    let mut p = hms.split(':');
    let (h, m) = (numero(p.next()?, 2)?, numero(p.next()?, 2)?);
    let seg = match p.next() {
        Some(x) => numero(x, 2)?,
        None => 0,
    };
    if p.next().is_some()
        || h > 23
        || m > 59
        || seg > 59
        || fracao.len() > 6
        || !fracao.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let f: i64 = if fracao.is_empty() {
        0
    } else {
        format!("{fracao:0<6}").parse().ok()?
    };
    micros += ((h * 60 + m - deslocamento) * 60 + seg) * 1_000_000 + f;
    Some(micros)
}

/// Escreve em ISO, em UTC: `2026-11-23 14:30:00` (com a fração só se houver).
pub fn para_texto(micros: i64) -> String {
    let dias = micros.div_euclid(MICROS_DIA);
    let resto = micros.rem_euclid(MICROS_DIA);
    let (ano, mes, dia) = data_de_dias(dias);
    let (seg_total, f) = (resto / 1_000_000, resto % 1_000_000);
    let (h, m, s) = (seg_total / 3600, seg_total / 60 % 60, seg_total % 60);
    let base = format!("{ano:04}-{mes:02}-{dia:02} {h:02}:{m:02}:{s:02}");
    if f == 0 {
        base
    } else {
        format!("{base}.{f:06}")
    }
}

/// Agora, em microssegundos desde 1970 (UTC).
pub fn agora() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn le_e_escreve() {
        assert_eq!(de_texto("1970-01-01"), Some(0));
        assert_eq!(
            de_texto("1970-01-02 00:00:01"),
            Some(MICROS_DIA + 1_000_000)
        );
        assert_eq!(
            para_texto(de_texto("2026-11-23T14:30:05Z").unwrap()),
            "2026-11-23 14:30:05"
        );
        assert_eq!(
            para_texto(de_texto("2026-11-23 14:30").unwrap()),
            "2026-11-23 14:30:00"
        );
        assert_eq!(
            para_texto(de_texto("2026-11-23T11:30:00-03:00").unwrap()),
            "2026-11-23 14:30:00"
        );
        assert_eq!(
            para_texto(de_texto("2026-11-23T14:30:00.5").unwrap()),
            "2026-11-23 14:30:00.500000"
        );
        assert_eq!(
            para_texto(de_texto("2024-02-29").unwrap()),
            "2024-02-29 00:00:00"
        );
        assert_eq!(
            para_texto(de_texto("1900-03-01").unwrap()),
            "1900-03-01 00:00:00"
        );
        assert_eq!(para_texto(-1), "1969-12-31 23:59:59.999999");
        // 2026-11-23 00:00:00 UTC conferido com `date -u -d 2026-11-23 +%s`.
        assert_eq!(de_texto("2026-11-23"), Some(1_795_392_000_000_000));
        for ruim in [
            "",
            "2026-13-01",
            "2026-02-30",
            "2025-02-29",
            "2026-1-1",
            "2026-11-23 24:00",
            "2026-11-23x",
            "ontem",
            "2026-11-23 10:00:00.1234567",
            "2026-11-23T10:00+15:00",
        ] {
            assert_eq!(de_texto(ruim), None, "{ruim}");
        }
    }

    #[test]
    fn todo_dia_vai_e_volta() {
        // 1600 a 2400: cada dia convertido para data e de volta.
        let mut anterior = (0, 0, 0);
        for dias in dias_de_data(1600, 1, 1)..dias_de_data(2400, 12, 31) {
            let (a, m, d) = data_de_dias(dias);
            assert_eq!(dias_de_data(a, m, d), dias);
            assert!(
                (1..=12).contains(&m) && d >= 1 && d <= dias_no_mes(a, m),
                "{a}-{m}-{d}"
            );
            assert!((a, m, d) > anterior);
            anterior = (a, m, d);
        }
        assert!(agora() > 1_700_000_000_000_000);
    }
}
