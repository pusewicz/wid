# frozen_string_literal: true

class ParserTest < Wid::Test
  def parse(string)
    tokenizer = Wid::Tokenizer.new(string)
    tokens = []
    while (token = tokenizer.next_token)
      tokens << token
    end

    parser = Wid::Parser.new(tokens)
    program = parser.parse

    assert_instance_of Wid::AST::ProgramNode, program
    assert_instance_of Wid::AST::StatementsNode, program.statements
    assert_instance_of Array, program.statements.body

    program.statements.to_h[:body]
  end

  def test_parses_numbers
    assert_equal [{type: :integer_node, value: 3}], parse("3")
    assert_equal [{type: :float_node, value: 2.5}], parse("2.5")
  end

  def test_parses_strings
    [%('a string 1'), %("a string 2")].each do |string|
      assert_equal [{
        type: :string_node,
        unescaped: string[1...-1]
      }], parse(string)
    end
  end

  def test_parses_booleans
    assert_equal [{type: :true_node}], parse("true")
    assert_equal [{type: :false_node}], parse("false")
  end

  def test_parses_nil
    assert_equal [{type: :nil_node}], parse("nil")
  end

  def test_parses_equality
    assert_equal [{
      type: :call_node,
      receiver: {type: :integer_node, value: 1},
      name: :==,
      arguments: {
        type: :arguments_node,
        arguments: [{type: :integer_node, value: 2}]
      },
      block: nil
    }], parse("1 == 2")

    assert_equal [{
      type: :call_node,
      receiver: {type: :integer_node, value: 1},
      name: :!=,
      arguments: {
        type: :arguments_node,
        arguments: [{type: :integer_node, value: 2}]
      },
      block: nil
    }], parse("1 != 2")
  end

  def test_parses_comparison
    assert_equal [{
      type: :call_node,
      receiver: {type: :integer_node, value: 1},
      name: :>,
      arguments: {
        type: :arguments_node,
        arguments: [{type: :integer_node, value: 2}]
      },
      block: nil
    }], parse("1 > 2")

    assert_equal [{
      type: :call_node,
      receiver: {type: :integer_node, value: 1},
      name: :>=,
      arguments: {
        type: :arguments_node,
        arguments: [{type: :integer_node, value: 2}]
      },
      block: nil
    }], parse("1 >= 2")

    assert_equal [{
      type: :call_node,
      receiver: {type: :integer_node, value: 1},
      name: :<,
      arguments: {
        type: :arguments_node,
        arguments: [{type: :integer_node, value: 2}]
      },
      block: nil
    }], parse("1 < 2")

    assert_equal [{
      type: :call_node,
      receiver: {type: :integer_node, value: 1},
      name: :<=,
      arguments: {
        type: :arguments_node,
        arguments: [{type: :integer_node, value: 2}]
      },
      block: nil
    }], parse("1 <= 2")
  end

  def test_parses_term
    assert_equal [{
      type: :call_node,
      receiver: {type: :integer_node, value: 1},
      name: :+,
      arguments: {
        type: :arguments_node,
        arguments: [{type: :integer_node, value: 2}]
      },
      block: nil
    }], parse("1 + 2")

    assert_equal [{
      type: :call_node,
      receiver: {type: :integer_node, value: 1},
      name: :-,
      arguments: {
        type: :arguments_node,
        arguments: [{type: :integer_node, value: 2}]
      },
      block: nil
    }], parse("1 - 2")
  end

  def test_parses_factor
    assert_equal [{
      type: :call_node,
      receiver: {type: :integer_node, value: 1},
      name: :*,
      arguments: {
        type: :arguments_node,
        arguments: [{type: :integer_node, value: 2}]
      },
      block: nil
    }], parse("1 * 2")

    assert_equal [{
      type: :call_node,
      receiver: {type: :integer_node, value: 1},
      name: :/,
      arguments: {
        type: :arguments_node,
        arguments: [{type: :integer_node, value: 2}]
      },
      block: nil
    }], parse("1 / 2")
  end

  def test_parses_unary
    assert_equal [{
      type: :unary_node,
      operator: :-,
      right: {type: :integer_node, value: 2}
    }], parse("-2")

    assert_equal [{
      type: :unary_node,
      operator: :!,
      right: {type: :integer_node, value: 2}
    }], parse("!2")

    assert_equal [{
      type: :unary_node,
      operator: :!,
      right: {
        type: :unary_node,
        operator: :!,
        right: {type: :integer_node, value: 2}
      }
    }], parse("!!2")
  end

  def test_parses_grouping
    assert_equal [{
      type: :grouping_node,
      expression: {type: :integer_node, value: 2}
    }], parse("(2)")
  end

  def test_parses_print_stmt
    assert_equal [{
      type: :print_node,
      expressions: [{type: :integer_node, value: 2}]
    }], parse("print 2")
  end

  def test_local_variable_write
    assert_equal [{
      type: :local_variable_write_node,
      name: :a,
      value: {type: :integer_node, value: 2}
    }], parse("a = 2")
  end
end
