module Wid
  module Nodes
    class FunctionCall < Expression
      attr_accessor :name, :arguments

      def initialize(name = nil, arguments = [])
        @name = name
        @arguments = arguments
      end

      def function_name
        @name.name
      end

      def ==(other)
        children == other&.children
      end

      def children
        [@name, @arguments]
      end
    end
  end
end
