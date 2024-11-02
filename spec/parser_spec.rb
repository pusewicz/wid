# frozen_string_literal: true

require_relative "spec_helper"

describe Wid::Parser do
  def parse(string)
    tokenizer = Wid::Tokenizer.new(string)
    tokens = []
    while (token = tokenizer.next_token)
      tokens << token
    end

    parser = Wid::Parser.new(tokens)
    program = parser.parse

    _(program).must_be_instance_of Wid::AST::ProgramNode
    _(program.statements).must_be_instance_of Wid::AST::StatementsNode
    _(program.statements.body).must_be_instance_of Array

    program.statements.to_h[:body]
  end

  it "parses numbers" do
    _(parse("3")).must_equal [{type: :integer_node, value: 3}]
    _(parse("2.5")).must_equal [{type: :float_node, value: 2.5}]
  end

  it "parses strings" do
    [%('a string 1'), %("a string 2")].each do |string|
      _(parse(string)).must_equal [{
        type: :string_node,
        unescaped: string[1...-1]
      }]
    end
  end

  it "parses booleans" do
    _(parse("true")).must_equal([{type: :true_node}])
    _(parse("false")).must_equal([{type: :false_node}])
  end

  it "parses nil" do
    _(parse("nil")).must_equal [{
      type: :nil_node
    }]
  end

  it "parses equality" do
    _(parse("1 == 2")).must_equal [{
      type: :call_node,
      receiver: {type: :integer_node, value: 1},
      name: :==,
      arguments: {
        type: :arguments_node,
        arguments: [{type: :integer_node, value: 2}]
      },
      block: nil
    }]

    _(parse("1 != 2")).must_equal [{
      type: :call_node,
      receiver: {type: :integer_node, value: 1},
      name: :!=,
      arguments: {
        type: :arguments_node,
        arguments: [{type: :integer_node, value: 2}]
      },
      block: nil
    }]
  end

  it "parses comparison" do
    _(parse("1 > 2")).must_equal [{
      type: :call_node,
      receiver: {type: :integer_node, value: 1},
      name: :>,
      arguments: {
        type: :arguments_node,
        arguments: [{type: :integer_node, value: 2}]
      },
      block: nil
    }]

    _(parse("1 >= 2")).must_equal [{
      type: :call_node,
      receiver: {type: :integer_node, value: 1},
      name: :>=,
      arguments: {
        type: :arguments_node,
        arguments: [{type: :integer_node, value: 2}]
      },
      block: nil
    }]

    _(parse("1 < 2")).must_equal [{
      type: :call_node,
      receiver: {type: :integer_node, value: 1},
      name: :<,
      arguments: {
        type: :arguments_node,
        arguments: [{type: :integer_node, value: 2}]
      },
      block: nil
    }]

    _(parse("1 <= 2")).must_equal [{
      type: :call_node,
      receiver: {type: :integer_node, value: 1},
      name: :<=,
      arguments: {
        type: :arguments_node,
        arguments: [{type: :integer_node, value: 2}]
      },
      block: nil
    }]
  end

  it "parses term" do
    _(parse("1 + 2")).must_equal [{
      type: :call_node,
      receiver: {type: :integer_node, value: 1},
      name: :+,
      arguments: {
        type: :arguments_node,
        arguments: [{type: :integer_node, value: 2}]
      },
      block: nil
    }]

    _(parse("1 - 2")).must_equal [{
      type: :call_node,
      receiver: {type: :integer_node, value: 1},
      name: :-,
      arguments: {
        type: :arguments_node,
        arguments: [{type: :integer_node, value: 2}]
      },
      block: nil
    }]
  end

  it "parses factor" do
    _(parse("1 * 2")).must_equal [{
      type: :call_node,
      receiver: {type: :integer_node, value: 1},
      name: :*,
      arguments: {
        type: :arguments_node,
        arguments: [{type: :integer_node, value: 2}]
      },
      block: nil
    }]

    _(parse("1 / 2")).must_equal [{
      type: :call_node,
      receiver: {type: :integer_node, value: 1},
      name: :/,
      arguments: {
        type: :arguments_node,
        arguments: [{type: :integer_node, value: 2}]
      },
      block: nil
    }]
  end

  it "parses unary" do
    _(parse("-2")).must_equal [{
      type: :unary_node,
      operator: :-,
      right: {type: :integer_node, value: 2}
    }]

    _(parse("!2")).must_equal [{
      type: :unary_node,
      operator: :!,
      right: {type: :integer_node, value: 2}
    }]

    _(parse("!!2")).must_equal [{
      type: :unary_node,
      operator: :!,
      right: {
        type: :unary_node,
        operator: :!,
        right: {type: :integer_node, value: 2}
      }
    }]
  end

  it "parses grouping" do
    _(parse("(2)")).must_equal [{
      type: :grouping_node,
      expression: {type: :integer_node, value: 2}
    }]
  end
end
